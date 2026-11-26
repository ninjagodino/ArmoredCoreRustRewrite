//! A second [`FullscreenMaterial`] pass alongside [`crate::blur::ZoomBlur`].
//!
//! Bevy 0.19.1 stores the specialized pipeline id on the view as the non-generic
//! `FullscreenMaterialPipelineId`. Two `FullscreenMaterialPlugin`s on one camera overwrite that
//! component, so one pass draws with the other's pipeline and wgpu rejects the bind group because
//! the uniform sizes differ. This is a copy of `bevy_core_pipeline::fullscreen_material` with the
//! pipeline id (and bind group) made generic, so the heat-haze pass can live next to the existing
//! zoom-blur/tonemap pass.

use core::any::type_name;
use core::marker::PhantomData;

use bevy::app::{App, Plugin};
use bevy::asset::AssetServer;
use bevy::core_pipeline::fullscreen_material::FullscreenMaterial;
use bevy::core_pipeline::FullscreenShader;
use bevy::ecs::component::Component;
use bevy::ecs::entity::Entity;
use bevy::ecs::error::BevyError;
use bevy::ecs::query::With;
use bevy::ecs::resource::Resource;
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{
    ComponentUniforms, DynamicUniformIndex, ExtractComponentPlugin, UniformComponentPlugin,
};
use bevy::render::render_resource::{
    binding_types::{sampler, texture_2d, uniform_buffer},
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
    CachedRenderPipelineId, Canonical, ColorTargetState, ColorWrites, FragmentState, Operations,
    PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages,
    Specializer, SpecializerKey, TextureFormat, TextureSampleType, TextureView, TextureViewId,
    Variants,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::ShaderRef;
use bevy::utils::default;

#[derive(Default)]
pub struct ExtraFullscreenMaterialPlugin<T: FullscreenMaterial> {
    _marker: PhantomData<T>,
}

impl<T: FullscreenMaterial> Plugin for ExtraFullscreenMaterialPlugin<T> {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractComponentPlugin::<T>::default(),
            UniformComponentPlugin::<T>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_pipeline::<T>)
            .add_systems(
                Render,
                (
                    prepare_pipelines::<T>.in_set(RenderSystems::Prepare),
                    prepare_bind_groups::<T>.in_set(RenderSystems::PrepareBindGroups),
                ),
            );

        let system = T::schedule_configs(fullscreen_system::<T>.into_configs());
        render_app.add_systems(T::schedule(), system);
    }
}

#[derive(Resource)]
struct ExtraFullscreenPipeline<T: FullscreenMaterial> {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    variants: Variants<RenderPipeline, ExtraFullscreenSpecializer>,
    _marker: PhantomData<T>,
}

struct ExtraFullscreenSpecializer;

#[derive(PartialEq, Eq, Hash, Clone, Copy, SpecializerKey)]
struct ExtraFullscreenKey {
    target_format: TextureFormat,
}

impl Specializer<RenderPipeline> for ExtraFullscreenSpecializer {
    type Key = ExtraFullscreenKey;

    fn specialize(
        &self,
        key: Self::Key,
        descriptor: &mut RenderPipelineDescriptor,
    ) -> Result<Canonical<Self::Key>, BevyError> {
        let fragment = descriptor.fragment_mut()?;
        fragment.set_target(
            0,
            ColorTargetState {
                format: key.target_format,
                blend: None,
                write_mask: ColorWrites::ALL,
            },
        );
        Ok(key)
    }
}

fn init_pipeline<T: FullscreenMaterial>(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "extra_fullscreen_bind_group_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<T>(true),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor::default());
    let shader = match T::fragment_shader() {
        ShaderRef::Default => {
            unimplemented!(
                "FullscreenMaterial::fragment_shader() must not return ShaderRef::Default"
            )
        }
        ShaderRef::Handle(handle) => handle,
        ShaderRef::Path(path) => asset_server.load(path),
    };

    let desc = RenderPipelineDescriptor {
        label: Some(format!("extra_fullscreen_pipeline<{}>", type_name::<T>()).into()),
        layout: vec![layout.clone()],
        vertex: fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader,
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    };

    commands.insert_resource(ExtraFullscreenPipeline::<T> {
        layout,
        sampler,
        variants: Variants::new(ExtraFullscreenSpecializer, desc),
        _marker: PhantomData,
    });
}

#[derive(Component)]
struct ExtraFullscreenPipelineId<T: FullscreenMaterial>(CachedRenderPipelineId, PhantomData<T>);

fn prepare_pipelines<T: FullscreenMaterial>(
    mut commands: Commands,
    pipeline_cache: Res<PipelineCache>,
    mut pipeline: ResMut<ExtraFullscreenPipeline<T>>,
    views: Query<(Entity, &ExtractedView, Option<&T>), With<ExtractedCamera>>,
) -> Result<(), BevyError> {
    for (entity, view, material) in &views {
        if material.is_none() {
            commands
                .entity(entity)
                .remove::<ExtraFullscreenPipelineId<T>>();
            continue;
        }

        let pipeline_id = pipeline.variants.specialize(
            &pipeline_cache,
            ExtraFullscreenKey {
                target_format: view.target_format,
            },
        )?;

        commands
            .entity(entity)
            .insert(ExtraFullscreenPipelineId(pipeline_id, PhantomData::<T>));
    }

    Ok(())
}

/// Bind groups for both main textures; we can't know ahead of time which is source or destination.
#[derive(Component)]
struct ExtraFullscreenBindGroup<T: FullscreenMaterial> {
    a: (TextureViewId, BindGroup),
    b: (TextureViewId, BindGroup),
    _marker: PhantomData<T>,
}

fn prepare_bind_groups<T: FullscreenMaterial>(
    mut commands: Commands,
    mut view: Query<(
        Entity,
        &ViewTarget,
        Option<&mut ExtraFullscreenBindGroup<T>>,
        Option<&T>,
    )>,
    fullscreen_pipeline: Option<Res<ExtraFullscreenPipeline<T>>>,
    pipeline_cache: Res<PipelineCache>,
    data_uniforms: Res<ComponentUniforms<T>>,
    render_device: Res<RenderDevice>,
) {
    let Some(fullscreen_pipeline) = fullscreen_pipeline else {
        return;
    };
    let Some(settings_binding) = data_uniforms.uniforms().binding() else {
        return;
    };

    for (entity, view_target, mut maybe_bind_groups, material) in &mut view {
        if material.is_none() {
            commands
                .entity(entity)
                .remove::<ExtraFullscreenBindGroup<T>>();
            continue;
        }

        let main_texture_view = view_target.main_texture_view();
        let main_texture_other_view = view_target.main_texture_other_view();

        let create_bind_group = |texture: &TextureView| {
            (
                texture.id(),
                render_device.create_bind_group(
                    "extra_fullscreen_bind_group",
                    &pipeline_cache.get_bind_group_layout(&fullscreen_pipeline.layout),
                    &BindGroupEntries::sequential((
                        texture,
                        &fullscreen_pipeline.sampler,
                        settings_binding.clone(),
                    )),
                ),
            )
        };

        if let Some(bind_groups) = &mut maybe_bind_groups {
            if bind_groups.a.0 != main_texture_view.id() {
                bind_groups.a = create_bind_group(main_texture_view);
            }
            if bind_groups.b.0 != main_texture_other_view.id() {
                bind_groups.b = create_bind_group(main_texture_other_view);
            }
        } else {
            commands.entity(entity).insert(ExtraFullscreenBindGroup {
                a: create_bind_group(main_texture_view),
                b: create_bind_group(main_texture_other_view),
                _marker: PhantomData::<T>,
            });
        }
    }
}

fn fullscreen_system<T: FullscreenMaterial>(
    view: ViewQuery<(
        &ViewTarget,
        &DynamicUniformIndex<T>,
        &ExtraFullscreenBindGroup<T>,
        &ExtraFullscreenPipelineId<T>,
    )>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    let (view_target, settings_index, bind_groups, pipeline_id) = view.into_inner();

    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_id.0) else {
        return;
    };

    let post_process = view_target.post_process_write();
    let source = post_process.source;
    let destination = post_process.destination;

    let (_, bind_group) = if bind_groups.a.0 == source.id() {
        &bind_groups.a
    } else {
        &bind_groups.b
    };

    let pass_descriptor = RenderPassDescriptor {
        label: Some("extra_fullscreen_pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: destination,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };

    {
        let mut render_pass = ctx.command_encoder().begin_render_pass(&pass_descriptor);
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, bind_group, &[settings_index.index()]);
        render_pass.draw(0..3, 0..1);
    }
}
