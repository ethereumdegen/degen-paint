//! The `model.*` op catalog.

use dpaint_core::{resolve, resolve_one, DocId, Error, MeshId, OpCx, Project, Result};

pub mod anim;
pub mod materials;
pub mod meshes;
pub mod nodes;
pub mod scene;
pub mod validate;

/// Declare an op: id, one-line help, args type and the function that applies it.
macro_rules! declare_op {
    ($(#[$m:meta])* $name:ident, $id:literal, $about:literal, $args:ty, $f:path) => {
        $(#[$m])*
        pub struct $name;
        impl dpaint_core::Op for $name {
            fn id(&self) -> &'static str { $id }
            fn about(&self) -> &'static str { $about }
            fn schema(&self) -> serde_json::Value { dpaint_core::schema_for::<$args>() }
            fn modes(&self) -> &'static [dpaint_core::doc::DocKind] {
                &[dpaint_core::doc::DocKind::Model]
            }
            fn apply(
                &self,
                project: &mut dpaint_core::Project,
                args: serde_json::Value,
                cx: &mut dpaint_core::OpCx,
            ) -> dpaint_core::Result<dpaint_core::OpEffect> {
                let parsed: $args = dpaint_core::parse_args($id, args)?;
                $f(project, parsed, cx)
            }
        }
    };
}

pub(crate) use declare_op;

/// The document this op targets, checked to be a model document up front.
pub(crate) fn target_model(project: &Project, cx: &OpCx) -> Result<DocId> {
    let doc = cx.target_doc(project)?;
    project.model(&doc)?;
    Ok(doc)
}

/// Resolve a selector to exactly one object of the expected kind.
pub(crate) fn one_of_type(
    project: &Project,
    selector: &str,
    doc: &DocId,
    want: &str,
) -> Result<String> {
    let m = resolve_one(project, selector, Some(doc))?;
    if m.type_name != want {
        return Err(Error::Invalid(format!(
            "selector '{selector}' matched a {} named '{}' but this op needs a {want}",
            m.type_name, m.name
        )));
    }
    Ok(m.id)
}

/// Resolve a selector to every object of the expected kind it matches.
pub(crate) fn all_of_type(
    project: &Project,
    selector: &str,
    doc: &DocId,
    want: &str,
) -> Result<Vec<String>> {
    let ms = resolve(project, selector, Some(doc))?;
    let ids: Vec<String> = ms
        .into_iter()
        .filter(|m| m.type_name == want)
        .map(|m| m.id)
        .collect();
    if ids.is_empty() {
        return Err(Error::Invalid(format!(
            "selector '{selector}' matched no {want} in {doc}"
        )));
    }
    Ok(ids)
}

/// A free id derived from a name, kept unique against `taken`.
pub(crate) fn unique_id<T, F>(name: &str, from_name: F, taken: &[String]) -> T
where
    F: Fn(&str) -> T,
    T: std::fmt::Display,
{
    let candidate = from_name(name);
    if !taken.iter().any(|t| t == &candidate.to_string()) {
        return candidate;
    }
    let mut n = 2;
    loop {
        let c = from_name(&format!("{name} {n}"));
        if !taken.iter().any(|t| t == &c.to_string()) {
            return c;
        }
        n += 1;
    }
}

/// Rebuild a mesh, transform its triangles, and store the result as a baked buffer.
pub(crate) fn bake_mesh<F>(
    project: &mut Project,
    doc: &DocId,
    mesh_id: &MeshId,
    cx: &OpCx,
    edit: F,
) -> Result<crate::geom::MeshData>
where
    F: FnOnce(&mut crate::geom::MeshData) -> Result<Option<Vec<[f32; 4]>>>,
{
    let mut data = crate::build::build_mesh(project, doc, mesh_id, cx.assets)?;
    let tangents = edit(&mut data)?;
    if data.indices.is_empty() {
        return Err(Error::DegenerateGeometry(format!(
            "'{mesh_id}' would have no triangles left"
        )));
    }
    let asset = crate::build::bake(cx.assets, &data, tangents.as_deref())?;
    let model = project.model_mut(doc)?;
    let mesh = model
        .meshes
        .iter_mut()
        .find(|m| &m.id == mesh_id)
        .ok_or_else(|| Error::Invalid(format!("no mesh '{mesh_id}' in {doc}")))?;
    mesh.source = dpaint_core::doc::model::MeshSource::Buffer { asset };
    Ok(data)
}

/// Every op this crate contributes.
pub fn ops() -> Vec<Box<dyn dpaint_core::Op>> {
    vec![
        Box::new(nodes::NodeAdd),
        Box::new(nodes::NodeRemove),
        Box::new(nodes::NodeReparent),
        Box::new(nodes::NodeRename),
        Box::new(nodes::NodeSetTrs),
        Box::new(nodes::NodeLookAt),
        Box::new(meshes::MeshPrimitive),
        Box::new(meshes::MeshExtrude),
        Box::new(meshes::MeshRevolve),
        Box::new(meshes::MeshLoft),
        Box::new(meshes::MeshFromText),
        Box::new(meshes::MeshMerge),
        Box::new(meshes::MeshWeld),
        Box::new(meshes::MeshRecomputeNormals),
        Box::new(meshes::MeshGenerateTangents),
        Box::new(meshes::MeshGenerateUv),
        Box::new(meshes::MeshTransformBake),
        Box::new(meshes::MeshDecimate),
        Box::new(meshes::MeshImport),
        Box::new(materials::MaterialCreate),
        Box::new(materials::MaterialSetPbr),
        Box::new(materials::MaterialSetTexture),
        Box::new(materials::MaterialFromRasterDoc),
        Box::new(materials::MaterialAssign),
        Box::new(scene::LightAdd),
        Box::new(scene::LightSet),
        Box::new(scene::CameraAdd),
        Box::new(scene::CameraSet),
        Box::new(scene::SceneSetUpAxis),
        Box::new(scene::SceneCenter),
        Box::new(scene::SceneScaleToFit),
        Box::new(anim::AnimClipCreate),
        Box::new(anim::AnimTrackAdd),
        Box::new(anim::AnimKeyAdd),
        Box::new(anim::AnimKeyRemove),
        Box::new(anim::AnimSetInterpolation),
        Box::new(validate::ModelValidate),
    ]
}
