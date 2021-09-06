use crate::{
    resources::CameraManager,
    types::{Camera, CameraProjection, DirectionalLight, DirectionalLightHandle},
    util::{bind_merge::BindGroupBuilder, buffer::WrappedPotBuffer, registry::ResourceRegistry},
    INTERNAL_SHADOW_DEPTH_FORMAT, SHADOW_DIMENSIONS,
};
use glam::{Mat4, Vec3};
use rend3_types::{DirectionalLightChange, RawDirectionalLightHandle};
use std::{
    mem::{self, size_of},
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};
use wgpu::{
    BindGroup, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingType, Buffer,
    BufferBindingType, BufferUsages, Device, Extent3d, Queue, ShaderStages, TextureAspect, TextureDescriptor,
    TextureDimension, TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor, TextureViewDimension,
};

pub struct InternalDirectionalLight {
    pub inner: DirectionalLight,
    pub camera: CameraManager,
    pub shadow_tex: u32,
}

#[derive(Debug, Copy, Clone)]
#[repr(C, align(16))]
struct ShaderDirectionalLightBufferHeader {
    total_lights: u32,
}

unsafe impl bytemuck::Zeroable for ShaderDirectionalLightBufferHeader {}
unsafe impl bytemuck::Pod for ShaderDirectionalLightBufferHeader {}

#[derive(Debug, Copy, Clone)]
#[repr(C, align(16))]
struct ShaderDirectionalLight {
    pub view_proj: Mat4,
    pub color: Vec3,
    pub shadow_tex: u32,
    pub direction: Vec3,
}

unsafe impl bytemuck::Zeroable for ShaderDirectionalLight {}
unsafe impl bytemuck::Pod for ShaderDirectionalLight {}

pub struct DirectionalLightManager {
    buffer: WrappedPotBuffer,

    view: TextureView,
    layer_views: Vec<Arc<TextureView>>,

    bgl: BindGroupLayout,
    bg: BindGroup,

    registry: ResourceRegistry<InternalDirectionalLight, DirectionalLight>,
}
impl DirectionalLightManager {
    pub fn new(device: &Device) -> Self {
        let registry = ResourceRegistry::new();

        let buffer = WrappedPotBuffer::new(
            device,
            0,
            mem::size_of::<ShaderDirectionalLight>() as _,
            BufferUsages::STORAGE,
            Some("directional lights"),
        );

        let (view, layer_views) = create_shadow_texture(device, 1);

        let bgl = create_shadow_bgl(device);
        let bg = create_shadow_bg(device, &bgl, &buffer, &view);

        Self {
            buffer,
            view,
            layer_views,

            bgl,
            bg,

            registry,
        }
    }

    pub fn allocate(&self) -> DirectionalLightHandle {
        self.registry.allocate()
    }

    pub fn fill(&mut self, handle: &DirectionalLightHandle, light: DirectionalLight) {
        self.registry.insert(
            handle,
            InternalDirectionalLight {
                inner: light,
                camera: CameraManager::new(
                    Camera {
                        projection: CameraProjection::from_orthographic_direction(light.direction.into()),
                        ..Camera::default()
                    },
                    None,
                ),
                shadow_tex: self.registry.count() as u32,
            },
        );
    }

    pub fn get_mut(&mut self, handle: RawDirectionalLightHandle) -> &mut InternalDirectionalLight {
        self.registry.get_mut(handle)
    }

    pub fn get_layer_view_arc(&self, layer: u32) -> Arc<TextureView> {
        Arc::clone(&self.layer_views[layer as usize])
    }

    pub fn update_directional_light(&mut self, handle: RawDirectionalLightHandle, change: DirectionalLightChange) {
        let internal = self.registry.get_mut(handle);
        internal.inner.update_from_changes(change);
        if let Some(direction) = change.direction {
            internal.camera.set_data(
                Camera {
                    projection: CameraProjection::from_orthographic_direction(direction.into()),
                    ..Camera::default()
                },
                None,
            );
        }
    }

    pub fn get_bgl(&self) -> &BindGroupLayout {
        &self.bgl
    }

    pub fn get_bg(&self) -> &BindGroup {
        &self.bg
    }

    pub fn ready(&mut self, device: &Device, queue: &Queue) {
        profiling::scope!("Directional Light Ready");

        self.registry.remove_all_dead(|_, _, _| ());

        let registered_count = self.registry.count();
        let recreate_view = registered_count != self.layer_views.len() && registered_count != 0;
        if recreate_view {
            let (view, layer_views) = create_shadow_texture(device, registered_count as u32);
            self.view = view;
            self.layer_views = layer_views;
        }

        let registry = &self.registry;

        let size = self.registry.count() * size_of::<ShaderDirectionalLight>()
            + size_of::<ShaderDirectionalLightBufferHeader>();

        let mut buffer = Vec::with_capacity(size);
        buffer.extend_from_slice(bytemuck::bytes_of(&ShaderDirectionalLightBufferHeader {
            total_lights: registry.count() as u32,
        }));
        for light in registry.values() {
            buffer.extend_from_slice(bytemuck::bytes_of(&ShaderDirectionalLight {
                view_proj: light.camera.view_proj(),
                color: light.inner.color * light.inner.intensity,
                direction: light.inner.direction,
                shadow_tex: light.shadow_tex as u32,
            }));
        }

        let reallocated_buffer = self.buffer.write_to_buffer(device, queue, &buffer);

        if reallocated_buffer || recreate_view {
            self.bg = create_shadow_bg(device, &self.bgl, &self.buffer, &self.view);
        }
    }

    pub fn values(&self) -> impl Iterator<Item = &InternalDirectionalLight> {
        self.registry.values()
    }
}

fn create_shadow_texture(device: &Device, count: u32) -> (TextureView, Vec<Arc<TextureView>>) {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("shadow texture"),
        size: Extent3d {
            width: SHADOW_DIMENSIONS,
            height: SHADOW_DIMENSIONS,
            depth_or_array_layers: count,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: INTERNAL_SHADOW_DEPTH_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
    });

    let primary_view = texture.create_view(&TextureViewDescriptor {
        label: Some("shadow texture view"),
        format: None,
        dimension: Some(TextureViewDimension::D2Array),
        aspect: TextureAspect::All,
        base_mip_level: 0,
        mip_level_count: None,
        base_array_layer: 0,
        array_layer_count: None,
    });

    let layer_views: Vec<_> = (0..count)
        .map(|idx| {
            Arc::new(texture.create_view(&TextureViewDescriptor {
                label: Some(&format!("shadow texture layer {}", count)),
                format: None,
                dimension: Some(TextureViewDimension::D2),
                aspect: TextureAspect::All,
                base_mip_level: 0,
                mip_level_count: None,
                base_array_layer: idx,
                array_layer_count: NonZeroU32::new(1),
            }))
        })
        .collect();

    (primary_view, layer_views)
}

fn create_shadow_bgl(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("shadow bgl"),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(
                        (mem::size_of::<ShaderDirectionalLightBufferHeader>()
                            + mem::size_of::<ShaderDirectionalLight>()) as _,
                    ),
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Depth,
                    view_dimension: TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
        ],
    })
}

fn create_shadow_bg(device: &Device, bgl: &BindGroupLayout, buffer: &Buffer, view: &TextureView) -> BindGroup {
    BindGroupBuilder::new(Some("shadow bg"))
        .with_buffer(buffer)
        .with_texture_view(view)
        .build(device, bgl)
}