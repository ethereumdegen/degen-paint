//! `model.anim.*` — keyframe tracks with LINEAR, STEP and CUBICSPLINE samplers.

use super::{declare_op, one_of_type, target_model, unique_id};
use dpaint_core::doc::model::{AnimChannel, AnimKey, AnimPath, Animation, Interpolation, ModelDoc};
use dpaint_core::{AnimId, Error, NodeId, OpCx, OpEffect, Project, Result};
use serde::Deserialize;

/// Values per key for a path, tripled for CUBICSPLINE (in-tangent, value, out-tangent).
fn components(path: AnimPath, interpolation: Interpolation) -> usize {
    let base = match path {
        AnimPath::Rotation => 4,
        _ => 3,
    };
    match interpolation {
        Interpolation::CubicSpline => base * 3,
        _ => base,
    }
}

/// Animations are addressed by id or by name — they are not part of the selector space.
fn find_anim(model: &ModelDoc, key: &str) -> Result<usize> {
    model
        .animations
        .iter()
        .position(|a| a.id.as_str() == key || a.name == key)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "no animation '{key}'; have [{}]",
                model
                    .animations
                    .iter()
                    .map(|a| a.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

fn find_channel(anim: &Animation, node: &NodeId, path: AnimPath) -> Option<usize> {
    anim.channels
        .iter()
        .position(|c| &c.node == node && c.path == path)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipCreateArgs {
    /// Name of the clip; also seeds its id.
    pub name: String,
}

fn anim_clip_create(project: &mut Project, args: ClipCreateArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.animations.iter().map(|a| a.id.to_string()).collect();
    let id = unique_id(&args.name, |s| AnimId::from_name(s), &taken);
    model.animations.push(Animation {
        id: id.clone(),
        name: args.name,
        channels: Vec::new(),
    });
    Ok(OpEffect::changed(&doc).with_created(id.to_string()))
}

declare_op!(
    AnimClipCreate,
    "model.anim.clip-create",
    "Create an empty animation clip",
    ClipCreateArgs,
    anim_clip_create
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrackAddArgs {
    /// Animation id or name. Created if it does not exist yet.
    pub animation: String,
    /// Selector for the node the track drives.
    pub node: String,
    /// Which property the track animates.
    pub path: AnimPath,
    /// Sampler: LINEAR, STEP or CUBICSPLINE.
    #[serde(default)]
    pub interpolation: Interpolation,
}

fn anim_track_add(project: &mut Project, args: TrackAddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node = NodeId::from(one_of_type(project, &args.node, &doc, "node")?);
    let model = project.model_mut(&doc)?;
    let index = match find_anim(model, &args.animation) {
        Ok(i) => i,
        Err(_) => {
            let taken: Vec<String> = model.animations.iter().map(|a| a.id.to_string()).collect();
            let id = unique_id(&args.animation, |s| AnimId::from_name(s), &taken);
            model.animations.push(Animation {
                id,
                name: args.animation.clone(),
                channels: Vec::new(),
            });
            model.animations.len() - 1
        }
    };
    let anim = &mut model.animations[index];
    if let Some(existing) = find_channel(anim, &node, args.path) {
        anim.channels[existing].interpolation = args.interpolation;
        return Ok(OpEffect::changed(&doc).warn(
            "track-exists",
            node.to_string(),
            format!(
                "{:?} track already existed; its interpolation is now {:?}",
                args.path, args.interpolation
            ),
        ));
    }
    anim.channels.push(AnimChannel {
        node,
        path: args.path,
        interpolation: args.interpolation,
        keys: Vec::new(),
    });
    let id = anim.id.to_string();
    Ok(OpEffect::changed(&doc).with_created(id))
}

declare_op!(
    AnimTrackAdd,
    "model.anim.track-add",
    "Add a translation, rotation or scale track for a node to an animation clip",
    TrackAddArgs,
    anim_track_add
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyAddArgs {
    /// Animation id or name.
    pub animation: String,
    /// Selector for the node the track drives.
    pub node: String,
    /// Which property the key belongs to.
    pub path: AnimPath,
    /// Time in seconds.
    pub t: f32,
    /// Key value: 3 numbers for translation and scale, 4 for a rotation quaternion.
    /// A CUBICSPLINE track takes three times as many: in-tangent, value, out-tangent.
    pub value: Vec<f32>,
}

fn anim_key_add(project: &mut Project, args: KeyAddArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node = NodeId::from(one_of_type(project, &args.node, &doc, "node")?);
    if !args.t.is_finite() {
        return Err(Error::Invalid(format!("key time {} is not finite", args.t)));
    }
    if args.value.iter().any(|v| !v.is_finite()) {
        return Err(Error::Invalid("key values must be finite".into()));
    }
    let model = project.model_mut(&doc)?;
    let index = find_anim(model, &args.animation)?;
    let anim = &mut model.animations[index];
    let ch = find_channel(anim, &node, args.path).ok_or_else(|| {
        Error::Invalid(format!(
            "animation '{}' has no {:?} track for node '{}'; add one with model.anim.track-add",
            args.animation, args.path, node
        ))
    })?;
    let channel = &mut anim.channels[ch];
    let want = components(args.path, channel.interpolation);
    if args.value.len() != want {
        return Err(Error::Invalid(format!(
            "a {:?} {:?} key needs {want} values, got {}",
            channel.interpolation,
            args.path,
            args.value.len()
        )));
    }
    let mut effect = OpEffect::changed(&doc);
    if let Some(existing) = channel.keys.iter().position(|k| (k.t - args.t).abs() < 1e-6) {
        channel.keys[existing].v = args.value;
        effect = effect.warn(
            "key-replaced",
            node.to_string(),
            format!("a key already existed at t={}", args.t),
        );
    } else {
        channel.keys.push(AnimKey { t: args.t, v: args.value });
    }
    channel
        .keys
        .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    Ok(effect)
}

declare_op!(
    AnimKeyAdd,
    "model.anim.key-add",
    "Add or replace a keyframe on an animation track",
    KeyAddArgs,
    anim_key_add
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyRemoveArgs {
    /// Animation id or name.
    pub animation: String,
    /// Selector for the node the track drives.
    pub node: String,
    /// Which property the key belongs to.
    pub path: AnimPath,
    /// Time of the key to drop. Omit to clear every key on the track.
    #[serde(default)]
    pub t: Option<f32>,
}

fn anim_key_remove(project: &mut Project, args: KeyRemoveArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node = NodeId::from(one_of_type(project, &args.node, &doc, "node")?);
    let model = project.model_mut(&doc)?;
    let index = find_anim(model, &args.animation)?;
    let anim = &mut model.animations[index];
    let ch = find_channel(anim, &node, args.path).ok_or_else(|| {
        Error::Invalid(format!(
            "animation '{}' has no {:?} track for node '{}'",
            args.animation, args.path, node
        ))
    })?;
    let channel = &mut anim.channels[ch];
    let before = channel.keys.len();
    match args.t {
        Some(t) => channel.keys.retain(|k| (k.t - t).abs() >= 1e-6),
        None => channel.keys.clear(),
    }
    if channel.keys.len() == before {
        return Err(Error::Invalid(format!(
            "no key at t={:?} on that track",
            args.t
        )));
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    AnimKeyRemove,
    "model.anim.key-remove",
    "Remove one keyframe, or every keyframe, from an animation track",
    KeyRemoveArgs,
    anim_key_remove
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInterpolationArgs {
    /// Animation id or name.
    pub animation: String,
    /// Selector for the node the track drives.
    pub node: String,
    /// Which track to change.
    pub path: AnimPath,
    /// New sampler: LINEAR, STEP or CUBICSPLINE.
    pub interpolation: Interpolation,
}

fn anim_set_interpolation(
    project: &mut Project,
    args: SetInterpolationArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let node = NodeId::from(one_of_type(project, &args.node, &doc, "node")?);
    let model = project.model_mut(&doc)?;
    let index = find_anim(model, &args.animation)?;
    let anim = &mut model.animations[index];
    let ch = find_channel(anim, &node, args.path).ok_or_else(|| {
        Error::Invalid(format!(
            "animation '{}' has no {:?} track for node '{}'",
            args.animation, args.path, node
        ))
    })?;
    let channel = &mut anim.channels[ch];
    if channel.interpolation == args.interpolation {
        return Ok(OpEffect::changed(&doc));
    }
    // Switching to or from CUBICSPLINE changes how many numbers a key holds. Rather than
    // invent tangents or silently drop them, convert: flat tangents in, middle value out.
    let old = components(args.path, channel.interpolation);
    let new = components(args.path, args.interpolation);
    if old != new {
        let base = match args.path {
            AnimPath::Rotation => 4,
            _ => 3,
        };
        for key in &mut channel.keys {
            key.v = if new > old {
                let zeros = vec![0.0; base];
                let mut v = zeros.clone();
                v.extend_from_slice(&key.v);
                v.extend_from_slice(&zeros);
                v
            } else {
                key.v[base..base * 2].to_vec()
            };
        }
    }
    channel.interpolation = args.interpolation;
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    AnimSetInterpolation,
    "model.anim.set-interpolation",
    "Switch a track between LINEAR, STEP and CUBICSPLINE, converting its keys",
    SetInterpolationArgs,
    anim_set_interpolation
);
