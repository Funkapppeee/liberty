//! IV's colour grading as a screen pass after tonemapping. The three numbers come from
//! `timecyc.dat` (ColourCorrect, Desaturation, Gamma; see `timecyc.rs`), the maths from how IV's
//! own post shader ends (read from the FusionShaders disassembly of `rage_postfx`: behaviour only).
//! Left out: ColourAdd (zero in all but two keyframes) and Contrast (1.0 everywhere, and that
//! shader doesn't read it). `LIBERTY_NO_GRADE` turns the pass into a pass-through.

use bevy::asset::{load_internal_asset, weak_handle};
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::core_pipeline::fullscreen_vertex_shader::fullscreen_shader_vertex_state;
use bevy::ecs::query::QueryItem;
use bevy::prelude::*;
use bevy::render::extract_component::{ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin, UniformComponentPlugin};
use bevy::render::render_graph::{NodeRunError, RenderGraphApp, RenderGraphContext, RenderLabel, ViewNode, ViewNodeRunner};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::view::ViewTarget;
use bevy::render::RenderApp;

const SHADER: Handle<Shader> = weak_handle!("6c1f0c5e-3f0b-4b7e-9a51-1d3f6f0a9e21");

/// On the camera. `tint` rgb = ColourCorrect / 255; `params` x = Desaturation, y = Gamma.
#[derive(Component, Clone, Copy, ExtractComponent, ShaderType)]
pub struct Grade {
    pub tint: Vec4,
    pub params: Vec4,
}

impl Grade {
    /// Changes nothing: full colour, tint 128, gamma 1.
    pub const NEUTRAL: Grade = Grade { tint: Vec4::new(0.5, 0.5, 0.5, 0.0), params: Vec4::new(1.0, 1.0, 0.0, 0.0) };
}

pub struct GradePlugin;

impl Plugin for GradePlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER, "grade.wgsl", Shader::from_wgsl);
        app.add_plugins((ExtractComponentPlugin::<Grade>::default(), UniformComponentPlugin::<Grade>::default()));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_render_graph_node::<ViewNodeRunner<GradeNode>>(Core3d, GradeLabel)
            .add_render_graph_edges(Core3d, (Node3d::Tonemapping, GradeLabel, Node3d::EndMainPassPostProcessing));
    }

    fn finish(&self, app: &mut App) {
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.init_resource::<GradePipeline>();
        }
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct GradeLabel;

#[derive(Default)]
struct GradeNode;

impl ViewNode for GradeNode {
    type ViewQuery = (&'static ViewTarget, &'static Grade, &'static DynamicUniformIndex<Grade>);

    fn run(&self, _graph: &mut RenderGraphContext, ctx: &mut RenderContext, (target, _grade, index): QueryItem<Self::ViewQuery>, world: &World) -> Result<(), NodeRunError> {
        let pipe = world.resource::<GradePipeline>();
        let Some(pipeline) = world.resource::<PipelineCache>().get_render_pipeline(pipe.id) else { return Ok(()) };
        let Some(uniforms) = world.resource::<ComponentUniforms<Grade>>().uniforms().binding() else { return Ok(()) };
        let post = target.post_process_write();
        let bind = ctx.render_device().create_bind_group("grade_bind_group", &pipe.layout, &BindGroupEntries::sequential((post.source, &pipe.sampler, uniforms.clone())));
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("grade_pass"),
            color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_render_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[index.index()]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

#[derive(Resource)]
struct GradePipeline {
    layout: BindGroupLayout,
    sampler: Sampler,
    id: CachedRenderPipelineId,
}

impl FromWorld for GradePipeline {
    fn from_world(world: &mut World) -> Self {
        let device = world.resource::<RenderDevice>();
        let layout = device.create_bind_group_layout(
            "grade_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::FRAGMENT,
                (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering), uniform_buffer::<Grade>(true)),
            ),
        );
        let sampler = device.create_sampler(&SamplerDescriptor::default());
        let id = world.resource_mut::<PipelineCache>().queue_render_pipeline(RenderPipelineDescriptor {
            label: Some("grade_pipeline".into()),
            layout: vec![layout.clone()],
            vertex: fullscreen_shader_vertex_state(),
            fragment: Some(FragmentState {
                shader: SHADER,
                shader_defs: vec![],
                entry_point: "fragment".into(),
                // the camera is HDR (for bloom), so the frame is still a float texture here
                targets: vec![Some(ColorTargetState { format: ViewTarget::TEXTURE_FORMAT_HDR, blend: None, write_mask: ColorWrites::ALL })],
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            push_constant_ranges: vec![],
            zero_initialize_workgroup_memory: false,
        });
        GradePipeline { layout, sampler, id }
    }
}
