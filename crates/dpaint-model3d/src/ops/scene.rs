//! `model.light.*`, `model.camera.*` and `model.scene.*`.

use super::{all_of_type, declare_op, target_model, unique_id};
use crate::export::scene_bounds;
use dpaint_core::doc::model::{Camera, Light, LightKind, Node, UpAxis};
use dpaint_core::{
    CameraId, Color, Error, LightId, NodeId, OpCx, OpEffect, Project, Result,
};
use serde::Deserialize;

fn color_of(project: &Project, s: &str) -> Result<Color> {
    if let Some(c) = project.palette.get(s) {
        return Ok(*c);
    }
    Color::parse(s).ok_or_else(|| {
        Error::Invalid(format!(
            "'{s}' is neither a palette entry nor a hex color like #ffddaa"
        ))
    })
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightAddArgs {
    /// Name of the light; also seeds its id.
    pub name: String,
    /// directional, point or spot.
    pub kind: LightKind,
    /// Light color, as a hex string or a palette name.
    #[serde(default)]
    pub color: Option<String>,
    /// Intensity: lux for directional lights, candela for point and spot.
    #[serde(default = "one")]
    pub intensity: f32,
    /// Distance at which the light is considered to have fallen to zero.
    #[serde(default)]
    pub range: Option<f32>,
    /// Also create a node carrying the light.
    #[serde(default = "yes")]
    pub node: bool,
    /// Node position.
    #[serde(default)]
    pub translation: Option<[f32; 3]>,
    /// Point the light's -Z axis at this position.
    #[serde(default)]
    pub look_at: Option<[f32; 3]>,
}

fn one() -> f32 {
    1.0
}

fn light_add(project: &mut Project, args: LightAddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let color = match &args.color {
        Some(c) => color_of(project, c)?,
        None => Color::WHITE,
    };
    if !(args.intensity.is_finite() && args.intensity >= 0.0) {
        return Err(Error::Invalid(format!(
            "light intensity must be >= 0, got {}",
            args.intensity
        )));
    }
    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.lights.iter().map(|l| l.id.to_string()).collect();
    let id = unique_id(&args.name, |s| LightId::from_name(s), &taken);
    model.lights.push(Light {
        id: id.clone(),
        name: args.name.clone(),
        kind: args.kind,
        color,
        intensity: args.intensity,
        range: args.range,
    });
    let mut effect = OpEffect::changed(&doc).with_created(id.to_string());
    if args.node {
        let taken: Vec<String> = model.nodes.iter().map(|n| n.id.to_string()).collect();
        let node_id = unique_id(&args.name, |s| NodeId::from_name(s), &taken);
        let mut node = Node::new(node_id.clone(), args.name.clone());
        node.light = Some(id);
        if let Some(t) = args.translation {
            node.translation = t;
        }
        if let Some(at) = args.look_at {
            node.rotation = crate::geom::look_rotation(
                crate::geom::sub(at, node.translation),
                [0.0, 1.0, 0.0],
            );
        }
        model.nodes.push(node);
        effect = effect.with_created(node_id.to_string());
    }
    Ok(effect)
}

declare_op!(
    LightAdd,
    "model.light.add",
    "Add a punctual light (KHR_lights_punctual) and optionally a node to carry it",
    LightAddArgs,
    light_add
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightSetArgs {
    /// Selector for the light(s) to change.
    pub target: String,
    /// New kind.
    #[serde(default)]
    pub kind: Option<LightKind>,
    /// New color, as a hex string or a palette name.
    #[serde(default)]
    pub color: Option<String>,
    /// New intensity.
    #[serde(default)]
    pub intensity: Option<f32>,
    /// New range.
    #[serde(default)]
    pub range: Option<f32>,
}

fn light_set(project: &mut Project, args: LightSetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "light")?;
    let color = args
        .color
        .as_deref()
        .map(|c| color_of(project, c))
        .transpose()?;
    if let Some(i) = args.intensity {
        if !(i.is_finite() && i >= 0.0) {
            return Err(Error::Invalid(format!("intensity must be >= 0, got {i}")));
        }
    }
    let model = project.model_mut(&doc)?;
    for id in &ids {
        let light = model
            .lights
            .iter_mut()
            .find(|l| l.id.as_str() == id)
            .ok_or_else(|| Error::Invalid(format!("no light '{id}'")))?;
        if let Some(k) = args.kind {
            light.kind = k;
        }
        if let Some(c) = color {
            light.color = c;
        }
        if let Some(i) = args.intensity {
            light.intensity = i;
        }
        if let Some(r) = args.range {
            light.range = Some(r);
        }
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    LightSet,
    "model.light.set",
    "Change a light's kind, color, intensity or range",
    LightSetArgs,
    light_set
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraAddArgs {
    /// Name of the camera; also seeds its id.
    pub name: String,
    /// Vertical field of view in degrees.
    #[serde(default = "default_fov")]
    pub fov: f32,
    /// Near clipping distance.
    #[serde(default = "default_znear")]
    pub znear: f32,
    /// Far clipping distance; omit for an infinite projection.
    #[serde(default)]
    pub zfar: Option<f32>,
    /// Also create a node carrying the camera.
    #[serde(default = "yes")]
    pub node: bool,
    /// Node position.
    #[serde(default)]
    pub translation: Option<[f32; 3]>,
    /// Aim the camera's -Z axis at this position.
    #[serde(default)]
    pub look_at: Option<[f32; 3]>,
}

fn default_fov() -> f32 {
    35.0
}

fn default_znear() -> f32 {
    0.01
}

fn camera_add(project: &mut Project, args: CameraAddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    if !(args.fov > 0.0 && args.fov < 180.0) {
        return Err(Error::Invalid(format!(
            "camera fov must be between 0 and 180 degrees, got {}",
            args.fov
        )));
    }
    if !(args.znear > 0.0 && args.znear.is_finite()) {
        return Err(Error::Invalid(format!(
            "camera znear must be positive, got {}",
            args.znear
        )));
    }
    if let Some(f) = args.zfar {
        if f <= args.znear {
            return Err(Error::Invalid(format!(
                "camera zfar ({f}) must exceed znear ({})",
                args.znear
            )));
        }
    }
    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.cameras.iter().map(|c| c.id.to_string()).collect();
    let id = unique_id(&args.name, |s| CameraId::from_name(s), &taken);
    model.cameras.push(Camera {
        id: id.clone(),
        name: args.name.clone(),
        yfov: args.fov.to_radians(),
        znear: args.znear,
        zfar: args.zfar,
    });
    let mut effect = OpEffect::changed(&doc).with_created(id.to_string());
    if args.node {
        let taken: Vec<String> = model.nodes.iter().map(|n| n.id.to_string()).collect();
        let node_id = unique_id(&args.name, |s| NodeId::from_name(s), &taken);
        let mut node = Node::new(node_id.clone(), args.name.clone());
        node.camera = Some(id);
        if let Some(t) = args.translation {
            node.translation = t;
        }
        if let Some(at) = args.look_at {
            node.rotation = crate::geom::look_rotation(
                crate::geom::sub(at, node.translation),
                [0.0, 1.0, 0.0],
            );
        }
        model.nodes.push(node);
        effect = effect.with_created(node_id.to_string());
    }
    Ok(effect)
}

declare_op!(
    CameraAdd,
    "model.camera.add",
    "Add a perspective camera and optionally a node to carry it",
    CameraAddArgs,
    camera_add
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraSetArgs {
    /// Selector for the camera(s) to change.
    pub target: String,
    /// Vertical field of view in degrees.
    #[serde(default)]
    pub fov: Option<f32>,
    /// Near clipping distance.
    #[serde(default)]
    pub znear: Option<f32>,
    /// Far clipping distance.
    #[serde(default)]
    pub zfar: Option<f32>,
}

fn camera_set(project: &mut Project, args: CameraSetArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "camera")?;
    if let Some(f) = args.fov {
        if !(f > 0.0 && f < 180.0) {
            return Err(Error::Invalid(format!(
                "camera fov must be between 0 and 180 degrees, got {f}"
            )));
        }
    }
    if let Some(n) = args.znear {
        if !(n > 0.0 && n.is_finite()) {
            return Err(Error::Invalid(format!("znear must be positive, got {n}")));
        }
    }
    let model = project.model_mut(&doc)?;
    for id in &ids {
        let cam = model
            .cameras
            .iter_mut()
            .find(|c| c.id.as_str() == id)
            .ok_or_else(|| Error::Invalid(format!("no camera '{id}'")))?;
        if let Some(f) = args.fov {
            cam.yfov = f.to_radians();
        }
        if let Some(n) = args.znear {
            cam.znear = n;
        }
        if let Some(f) = args.zfar {
            cam.zfar = Some(f);
        }
        if let Some(f) = cam.zfar {
            if f <= cam.znear {
                return Err(Error::Invalid(format!(
                    "camera '{id}' would end up with zfar ({f}) at or behind znear ({})",
                    cam.znear
                )));
            }
        }
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    CameraSet,
    "model.camera.set",
    "Change a camera's field of view or clipping planes",
    CameraSetArgs,
    camera_set
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpAxisArgs {
    /// `y` (glTF native) or `z` (CAD style). Export rotates a `z` scene into Y-up.
    pub axis: UpAxis,
}

fn scene_set_up_axis(project: &mut Project, args: UpAxisArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    project.model_mut(&doc)?.up_axis = args.axis;
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    SceneSetUpAxis,
    "model.scene.set-up-axis",
    "Declare whether the scene is authored Y-up or Z-up",
    UpAxisArgs,
    scene_set_up_axis
);

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CenterMode {
    /// Move the bounding-box centre to the origin.
    #[default]
    Bbox,
    /// Centre on X and Z, and rest the bottom of the scene on Y = 0.
    Ground,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CenterArgs {
    /// How to centre.
    #[serde(default)]
    pub mode: CenterMode,
}

fn scene_center(project: &mut Project, args: CenterArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let Some((lo, hi)) = scene_bounds(project, &doc, cx.assets)? else {
        return Ok(OpEffect::changed(&doc).warn(
            "empty-scene",
            doc.to_string(),
            "no drawable geometry to centre",
        ));
    };
    let mid = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    let shift = match args.mode {
        CenterMode::Bbox => [-mid[0], -mid[1], -mid[2]],
        CenterMode::Ground => [-mid[0], -lo[1], -mid[2]],
    };
    let model = project.model_mut(&doc)?;
    let roots: Vec<NodeId> = model.roots().iter().map(|n| n.id.clone()).collect();
    for id in roots {
        if let Some(n) = model.node_mut(&id) {
            n.translation = [
                n.translation[0] + shift[0],
                n.translation[1] + shift[1],
                n.translation[2] + shift[2],
            ];
        }
    }
    Ok(OpEffect::changed(&doc).with_data(serde_json::json!({
        "bounds": { "min": lo, "max": hi },
        "shift": shift,
    })))
}

declare_op!(
    SceneCenter,
    "model.scene.center",
    "Move the scene so its bounding box is centred on the origin",
    CenterArgs,
    scene_center
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaleToFitArgs {
    /// Target size of the scene's largest dimension, in scene units.
    pub size: f32,
}

fn scene_scale_to_fit(
    project: &mut Project,
    args: ScaleToFitArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    if !(args.size.is_finite() && args.size > 0.0) {
        return Err(Error::Invalid(format!(
            "target size must be positive, got {}",
            args.size
        )));
    }
    let Some((lo, hi)) = scene_bounds(project, &doc, cx.assets)? else {
        return Ok(OpEffect::changed(&doc).warn(
            "empty-scene",
            doc.to_string(),
            "no drawable geometry to scale",
        ));
    };
    let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(hi[2] - lo[2]);
    if extent <= 1e-9 {
        return Err(Error::DegenerateGeometry(
            "scene has no measurable extent to scale".into(),
        ));
    }
    let factor = args.size / extent;
    let model = project.model_mut(&doc)?;
    let roots: Vec<NodeId> = model.roots().iter().map(|n| n.id.clone()).collect();
    for id in roots {
        if let Some(n) = model.node_mut(&id) {
            n.scale = [n.scale[0] * factor, n.scale[1] * factor, n.scale[2] * factor];
            n.translation = [
                n.translation[0] * factor,
                n.translation[1] * factor,
                n.translation[2] * factor,
            ];
        }
    }
    Ok(OpEffect::changed(&doc).with_data(serde_json::json!({
        "extent": extent,
        "factor": factor,
    })))
}

declare_op!(
    SceneScaleToFit,
    "model.scene.scale-to-fit",
    "Uniformly scale the scene so its largest dimension matches a target size",
    ScaleToFitArgs,
    scene_scale_to_fit
);
