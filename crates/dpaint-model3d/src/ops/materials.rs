//! `model.material.*` — PBR metallic-roughness materials and their texture bindings.

use super::{all_of_type, declare_op, one_of_type, target_model, unique_id};
use dpaint_core::doc::model::{AlphaMode, Material, TextureBinding, TextureSlot, TextureSource};
use dpaint_core::{
    AssetRef, Color, Error, MaterialId, NodeId, OpCx, OpEffect, Project, Result,
};
use serde::Deserialize;

/// Colors accept a palette name or any CSS-style hex, so `--base-color brand-red` works
/// wherever `#c0392b` does.
fn color_of(project: &Project, s: &str) -> Result<Color> {
    if let Some(c) = project.palette.get(s) {
        return Ok(*c);
    }
    Color::parse(s).ok_or_else(|| {
        Error::Invalid(format!(
            "'{s}' is neither a palette entry nor a hex color like #c0392b"
        ))
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PbrArgs {
    /// Base color, as a hex string or a palette name.
    #[serde(default)]
    pub base_color: Option<String>,
    /// Metalness, 0..1.
    #[serde(default)]
    pub metallic: Option<f32>,
    /// Roughness, 0..1.
    #[serde(default)]
    pub roughness: Option<f32>,
    /// Emissive color, as a hex string or a palette name.
    #[serde(default)]
    pub emissive: Option<String>,
    /// Emissive multiplier; values above 1 export as KHR_materials_emissive_strength.
    #[serde(default)]
    pub emissive_strength: Option<f32>,
    /// Disable back-face culling.
    #[serde(default)]
    pub double_sided: Option<bool>,
    /// OPAQUE, MASK or BLEND.
    #[serde(default)]
    pub alpha_mode: Option<AlphaMode>,
}

impl PbrArgs {
    fn apply_to(&self, project: &Project, m: &mut Material) -> Result<()> {
        if let Some(c) = &self.base_color {
            m.base_color = color_of(project, c)?;
        }
        if let Some(v) = self.metallic {
            if !(0.0..=1.0).contains(&v) {
                return Err(Error::Invalid(format!("metallic must be 0..1, got {v}")));
            }
            m.metallic = v;
        }
        if let Some(v) = self.roughness {
            if !(0.0..=1.0).contains(&v) {
                return Err(Error::Invalid(format!("roughness must be 0..1, got {v}")));
            }
            m.roughness = v;
        }
        if let Some(c) = &self.emissive {
            m.emissive = Some(color_of(project, c)?);
        }
        if let Some(v) = self.emissive_strength {
            if !(v.is_finite() && v >= 0.0) {
                return Err(Error::Invalid(format!(
                    "emissive_strength must be >= 0, got {v}"
                )));
            }
            m.emissive_strength = v;
        }
        if let Some(v) = self.double_sided {
            m.double_sided = v;
        }
        if let Some(v) = self.alpha_mode {
            m.alpha_mode = Some(v);
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MaterialCreateArgs {
    /// Name of the material; also seeds its id.
    pub name: String,
    #[serde(flatten)]
    pub pbr: PbrArgs,
}

fn material_create(
    project: &mut Project,
    args: MaterialCreateArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let mut material = Material::new(MaterialId::from("mat_placeholder"), args.name.clone());
    args.pbr.apply_to(project, &mut material)?;
    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.materials.iter().map(|m| m.id.to_string()).collect();
    material.id = unique_id(&args.name, |s| MaterialId::from_name(s), &taken);
    let id = material.id.to_string();
    model.materials.push(material);
    Ok(OpEffect::changed(&doc).with_created(id))
}

declare_op!(
    MaterialCreate,
    "model.material.create",
    "Create a PBR metallic-roughness material",
    MaterialCreateArgs,
    material_create
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MaterialSetPbrArgs {
    /// Selector for the material(s) to change.
    pub target: String,
    #[serde(flatten)]
    pub pbr: PbrArgs,
}

fn material_set_pbr(
    project: &mut Project,
    args: MaterialSetPbrArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let ids = all_of_type(project, &args.target, &doc, "material")?;
    // Validate against a copy before anything is written.
    let mut staged = Vec::with_capacity(ids.len());
    for id in &ids {
        let mut m = project
            .model(&doc)?
            .material(&MaterialId::from(id.as_str()))
            .ok_or_else(|| Error::Invalid(format!("no material '{id}'")))?
            .clone();
        args.pbr.apply_to(project, &mut m)?;
        staged.push(m);
    }
    let model = project.model_mut(&doc)?;
    for updated in staged {
        if let Some(slot) = model.materials.iter_mut().find(|m| m.id == updated.id) {
            *slot = updated;
        }
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MaterialSetPbr,
    "model.material.set-pbr",
    "Set base color, metallic, roughness, emissive, alpha mode or double-sidedness",
    MaterialSetPbrArgs,
    material_set_pbr
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTextureArgs {
    /// Selector for the material.
    pub target: String,
    /// Which map to bind.
    pub slot: TextureSlot,
    /// A raster document in this project, rendered to an image on export. The model
    /// document being edited is chosen with the global `--doc`.
    #[serde(default)]
    pub source: Option<String>,
    /// An asset reference (`blake3:....png`) holding an encoded PNG or JPEG.
    #[serde(default)]
    pub asset: Option<String>,
    /// Normal-map strength or occlusion strength, depending on the slot.
    #[serde(default = "one")]
    pub scale: f32,
    /// Which TEXCOORD set the map samples.
    #[serde(default)]
    pub uv_set: u32,
    /// Remove the binding in this slot instead of setting one.
    #[serde(default)]
    pub clear: bool,
}

fn one() -> f32 {
    1.0
}

fn material_set_texture(
    project: &mut Project,
    args: SetTextureArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let id = MaterialId::from(one_of_type(project, &args.target, &doc, "material")?);
    if args.clear {
        let model = project.model_mut(&doc)?;
        let m = model
            .materials
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or_else(|| Error::Invalid(format!("no material '{id}'")))?;
        let before = m.textures.len();
        m.textures.retain(|t| t.slot != args.slot);
        let mut effect = OpEffect::changed(&doc);
        if m.textures.len() == before {
            effect = effect.warn(
                "no-op",
                id.to_string(),
                format!("no {:?} texture was bound", args.slot),
            );
        }
        return Ok(effect);
    }
    let source = match (&args.source, &args.asset) {
        (Some(d), None) => {
            let doc_id = project.resolve_doc(Some(d))?;
            let kind = project.doc(&doc_id)?.kind();
            if project.doc(&doc_id)?.as_raster().is_none() {
                return Err(Error::WrongDocumentKind {
                    op: "model.material.set-texture".into(),
                    kind: kind.to_string(),
                });
            }
            if project.would_cycle(&doc, &doc_id) {
                return Err(Error::CyclicLink {
                    from: doc.to_string(),
                    to: doc_id.to_string(),
                });
            }
            TextureSource::Document { document: doc_id }
        }
        (None, Some(a)) => {
            let asset = AssetRef(a.clone());
            if !cx.assets.contains(&asset) {
                return Err(Error::AssetMissing(a.clone()));
            }
            TextureSource::Asset { asset }
        }
        _ => {
            return Err(Error::Invalid(
                "model.material.set-texture needs exactly one of `document` or `asset`".into(),
            ))
        }
    };
    let model = project.model_mut(&doc)?;
    let m = model
        .materials
        .iter_mut()
        .find(|m| m.id == id)
        .ok_or_else(|| Error::Invalid(format!("no material '{id}'")))?;
    m.textures.retain(|t| t.slot != args.slot);
    m.textures.push(TextureBinding {
        slot: args.slot,
        source,
        scale: args.scale,
        uv_set: args.uv_set,
    });
    m.textures.sort_by_key(|t| format!("{:?}", t.slot));
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MaterialSetTexture,
    "model.material.set-texture",
    "Bind a raster document or asset to a material's baseColor, normal, metallicRoughness, occlusion or emissive slot",
    SetTextureArgs,
    material_set_texture
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FromRasterDocArgs {
    /// The raster document to use as a texture.
    pub source: String,
    /// Name of the new material; defaults to the document's name.
    #[serde(default)]
    pub name: Option<String>,
    /// Which map the document supplies.
    #[serde(default = "base_color_slot")]
    pub slot: TextureSlot,
    #[serde(flatten)]
    pub pbr: PbrArgs,
}

fn base_color_slot() -> TextureSlot {
    TextureSlot::BaseColor
}

fn material_from_raster_doc(
    project: &mut Project,
    args: FromRasterDocArgs,
    cx: &mut OpCx,
) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let src = project.resolve_doc(Some(&args.source))?;
    let kind = project.doc(&src)?.kind();
    if project.doc(&src)?.as_raster().is_none() {
        return Err(Error::WrongDocumentKind {
            op: "model.material.from-raster-doc".into(),
            kind: kind.to_string(),
        });
    }
    if project.would_cycle(&doc, &src) {
        return Err(Error::CyclicLink {
            from: doc.to_string(),
            to: src.to_string(),
        });
    }
    let name = args
        .name
        .clone()
        .unwrap_or_else(|| project.doc(&src).map(|d| d.name().to_string()).unwrap_or_default());
    let mut material = Material::new(MaterialId::from("mat_placeholder"), name.clone());
    args.pbr.apply_to(project, &mut material)?;
    material.textures.push(TextureBinding {
        slot: args.slot,
        source: TextureSource::Document { document: src },
        scale: 1.0,
        uv_set: 0,
    });
    let model = project.model_mut(&doc)?;
    let taken: Vec<String> = model.materials.iter().map(|m| m.id.to_string()).collect();
    material.id = unique_id(&name, |s| MaterialId::from_name(s), &taken);
    let id = material.id.to_string();
    model.materials.push(material);
    Ok(OpEffect::changed(&doc).with_created(id))
}

declare_op!(
    MaterialFromRasterDoc,
    "model.material.from-raster-doc",
    "Create a material textured by a raster document in this project",
    FromRasterDocArgs,
    material_from_raster_doc
);

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssignArgs {
    /// Selector for the node(s) to paint.
    pub target: String,
    /// Selector for the material to assign. Omit with `clear` to remove the assignment.
    #[serde(default)]
    pub material: Option<String>,
    /// Remove the material assignment instead of setting one.
    #[serde(default)]
    pub clear: bool,
}

fn material_assign(project: &mut Project, args: AssignArgs, cx: &mut OpCx) -> Result<OpEffect> {
    let doc = target_model(project, cx)?;
    let nodes = all_of_type(project, &args.target, &doc, "node")?;
    let material = match (&args.material, args.clear) {
        (Some(m), false) => Some(MaterialId::from(one_of_type(project, m, &doc, "material")?)),
        (None, true) => None,
        _ => {
            return Err(Error::Invalid(
                "model.material.assign needs either `material` or `clear: true`".into(),
            ))
        }
    };
    let model = project.model_mut(&doc)?;
    for id in &nodes {
        let node = model
            .node_mut(&NodeId::from(id.as_str()))
            .ok_or_else(|| Error::Invalid(format!("no node '{id}'")))?;
        node.material = material.clone();
    }
    Ok(OpEffect::changed(&doc))
}

declare_op!(
    MaterialAssign,
    "model.material.assign",
    "Assign a material to nodes",
    AssignArgs,
    material_assign
);
