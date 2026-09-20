//! glTF 2.0 export: a `.gltf` + `.bin` pair and a packed `.glb`, from the same tables.

use crate::build::{build_mesh, world_transforms};
use crate::geom::MeshData;
use dpaint_core::doc::model::{
    AlphaMode, AnimPath, Interpolation, LightKind, Material, ModelDoc, TextureSlot, TextureSource,
    UpAxis,
};
use dpaint_core::{AssetStore, Color, DocId, Error, MaterialId, MeshId, NodeId, Project, Result};
use gltf_json as gj;
use gj::validation::Checked::Valid;
use gj::validation::USize64;
use std::collections::BTreeMap;

/// The three artifacts of an export. `json` references the buffer as `<doc-id>.bin`, so the
/// caller writes `bin` next to it under exactly that name; `glb` is self-contained.
#[derive(Debug, Clone, PartialEq)]
pub struct GltfOut {
    pub json: String,
    pub bin: Vec<u8>,
    pub glb: Vec<u8>,
}

impl GltfOut {
    /// File name the `.gltf` JSON expects the buffer to be written under.
    pub fn buffer_name(doc: &DocId) -> String {
        format!("{doc}.bin")
    }
}

/// Supplies the encoded image bytes for a [`TextureSource::Document`] binding: a raster
/// document in this project, rendered by the caller.
pub type TextureResolver<'a> = dyn Fn(&DocId) -> Result<Vec<u8>> + 'a;

const GLB_MAGIC: u32 = 0x4654_6C67;
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

#[derive(Default)]
struct Bin {
    data: Vec<u8>,
}

impl Bin {
    fn align(&mut self) {
        while self.data.len() % 4 != 0 {
            self.data.push(0);
        }
    }

    /// Append raw bytes at a 4-byte boundary, returning `(offset, length)`.
    fn push(&mut self, bytes: &[u8]) -> (usize, usize) {
        self.align();
        let off = self.data.len();
        self.data.extend_from_slice(bytes);
        (off, bytes.len())
    }

    fn push_f32(&mut self, values: &[f32]) -> (usize, usize) {
        let mut buf = Vec::with_capacity(values.len() * 4);
        for v in values {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        self.push(&buf)
    }

    fn push_u32(&mut self, values: &[u32]) -> (usize, usize) {
        let mut buf = Vec::with_capacity(values.len() * 4);
        for v in values {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        self.push(&buf)
    }
}

struct Ctx<'a> {
    root: gj::Root,
    bin: Bin,
    buffer: gj::Index<gj::Buffer>,
    textures: &'a TextureResolver<'a>,
    assets: &'a AssetStore,
    sampler: Option<gj::Index<gj::texture::Sampler>>,
    images: BTreeMap<String, gj::Index<gj::Texture>>,
    emissive_strength_used: bool,
}

impl Ctx<'_> {
    fn view(&mut self, off: usize, len: usize, target: Option<gj::buffer::Target>) -> gj::Index<gj::buffer::View> {
        let view = gj::buffer::View {
            buffer: self.buffer,
            byte_length: USize64::from(len),
            byte_offset: Some(USize64::from(off)),
            byte_stride: None,
            name: None,
            target: target.map(Valid),
            extensions: None,
            extras: Default::default(),
        };
        self.root.push(view)
    }

    // A glTF accessor genuinely has this many independent fields; bundling them into a
    // struct used at exactly three call sites would add indirection, not clarity.
    #[allow(clippy::too_many_arguments)]
    fn accessor(
        &mut self,
        view: gj::Index<gj::buffer::View>,
        count: usize,
        component: gj::accessor::ComponentType,
        type_: gj::accessor::Type,
        min: Option<serde_json::Value>,
        max: Option<serde_json::Value>,
        name: &str,
    ) -> gj::Index<gj::Accessor> {
        let acc = gj::Accessor {
            buffer_view: Some(view),
            byte_offset: Some(USize64(0)),
            count: USize64::from(count),
            component_type: Valid(gj::accessor::GenericComponentType(component)),
            extensions: None,
            extras: Default::default(),
            type_: Valid(type_),
            min,
            max,
            name: Some(name.to_string()),
            normalized: false,
            sparse: None,
        };
        self.root.push(acc)
    }

    fn f32_accessor(
        &mut self,
        values: &[f32],
        components: usize,
        type_: gj::accessor::Type,
        bounds: bool,
        target: Option<gj::buffer::Target>,
        name: &str,
    ) -> gj::Index<gj::Accessor> {
        let (off, len) = self.bin.push_f32(values);
        let view = self.view(off, len, target);
        let count = values.len() / components;
        let (min, max) = if bounds && count > 0 {
            let mut lo = vec![f32::MAX; components];
            let mut hi = vec![f32::MIN; components];
            for chunk in values.chunks_exact(components) {
                for (i, v) in chunk.iter().enumerate() {
                    lo[i] = lo[i].min(*v);
                    hi[i] = hi[i].max(*v);
                }
            }
            (
                Some(serde_json::to_value(&lo).unwrap_or(serde_json::Value::Null)),
                Some(serde_json::to_value(&hi).unwrap_or(serde_json::Value::Null)),
            )
        } else {
            (None, None)
        };
        self.accessor(
            view,
            count,
            gj::accessor::ComponentType::F32,
            type_,
            min,
            max,
            name,
        )
    }

    fn default_sampler(&mut self) -> gj::Index<gj::texture::Sampler> {
        if let Some(s) = self.sampler {
            return s;
        }
        let s = self.root.push(gj::texture::Sampler {
            mag_filter: Some(Valid(gj::texture::MagFilter::Linear)),
            min_filter: Some(Valid(gj::texture::MinFilter::LinearMipmapLinear)),
            name: None,
            wrap_s: Valid(gj::texture::WrappingMode::Repeat),
            wrap_t: Valid(gj::texture::WrappingMode::Repeat),
            extensions: None,
            extras: Default::default(),
        });
        self.sampler = Some(s);
        s
    }

    /// Image bytes for a binding, embedded as a buffer view so `.glb` and `.gltf` share one
    /// code path. Deduplicated by source key.
    fn texture(&mut self, source: &TextureSource) -> Result<gj::Index<gj::Texture>> {
        let key = match source {
            TextureSource::Document { document } => format!("doc:{document}"),
            TextureSource::Asset { asset } => format!("asset:{asset}"),
        };
        if let Some(i) = self.images.get(&key) {
            return Ok(*i);
        }
        let bytes = match source {
            TextureSource::Document { document } => (self.textures)(document)?,
            TextureSource::Asset { asset } => self.assets.get(asset)?,
        };
        let mime = sniff_mime(&bytes).ok_or_else(|| {
            Error::UnsupportedFormat(format!(
                "texture {key} is not a PNG or JPEG; glTF images must be one of those"
            ))
        })?;
        let (off, len) = self.bin.push(&bytes);
        let view = self.view(off, len, None);
        let image = self.root.push(gj::Image {
            buffer_view: Some(view),
            mime_type: Some(gj::image::MimeType(mime.to_string())),
            name: Some(key.clone()),
            uri: None,
            extensions: None,
            extras: Default::default(),
        });
        let sampler = self.default_sampler();
        let tex = self.root.push(gj::Texture {
            name: Some(key.clone()),
            sampler: Some(sampler),
            source: image,
            extensions: None,
            extras: Default::default(),
        });
        self.images.insert(key, tex);
        Ok(tex)
    }
}

fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Image dimensions straight out of the container header — enough for the texture-size lint
/// without pulling in a decoder.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) && bytes.len() >= 24 {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some((w, h));
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            let is_sof = (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC;
            if is_sof {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

fn linear(c: Color) -> [f32; 4] {
    c.to_linear()
}

/// Export a model document as glTF 2.0.
pub fn export(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    textures: &TextureResolver<'_>,
) -> Result<GltfOut> {
    let (mut root, bin, buffer) = build_root(project, doc, assets, textures)?;

    // Two serializations: the .gltf points at a sidecar file, the GLB's JSON chunk does not.
    let glb_json = root.to_string().map_err(Error::Json)?;
    if let Some(b) = root.buffers.get_mut(buffer.value()) {
        b.uri = Some(GltfOut::buffer_name(doc));
    }
    let json = root.to_string().map_err(Error::Json)?;
    let glb = pack_glb(glb_json.as_bytes(), &bin);
    Ok(GltfOut { json, bin, glb })
}

/// The whole glTF object graph plus its binary buffer, before serialization.
fn build_root(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    textures: &TextureResolver<'_>,
) -> Result<(gj::Root, Vec<u8>, gj::Index<gj::Buffer>)> {
    let model = project.model(doc)?;
    let mut root = gj::Root {
        asset: gj::Asset {
            version: "2.0".into(),
            generator: Some("degen-paint".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let buffer = root.push(gj::Buffer {
        byte_length: USize64(0),
        name: Some(GltfOut::buffer_name(doc)),
        uri: None,
        extensions: None,
        extras: Default::default(),
    });
    let mut cx = Ctx {
        root,
        bin: Bin::default(),
        buffer,
        textures,
        assets,
        sampler: None,
        images: BTreeMap::new(),
        emissive_strength_used: false,
    };

    // Materials first: a primitive needs its index, and a normal map decides whether the
    // mesh carries tangents.
    let mut material_index: BTreeMap<MaterialId, gj::Index<gj::Material>> = BTreeMap::new();
    let mut needs_tangents: BTreeMap<MaterialId, bool> = BTreeMap::new();
    for m in &model.materials {
        let (idx, normal_mapped) = export_material(&mut cx, m)?;
        material_index.insert(m.id.clone(), idx);
        needs_tangents.insert(m.id.clone(), normal_mapped);
    }

    // Meshes, keyed by (geometry, material) because glTF binds a material per primitive.
    let mut mesh_index: BTreeMap<(MeshId, Option<MaterialId>), gj::Index<gj::Mesh>> =
        BTreeMap::new();
    let mut node_index: BTreeMap<NodeId, gj::Index<gj::Node>> = BTreeMap::new();
    let mut light_index: BTreeMap<String, u32> = BTreeMap::new();
    let mut camera_index: BTreeMap<String, gj::Index<gj::Camera>> = BTreeMap::new();

    for cam in &model.cameras {
        let idx = cx.root.push(gj::Camera {
            name: Some(cam.name.clone()),
            orthographic: None,
            perspective: Some(gj::camera::Perspective {
                aspect_ratio: None,
                yfov: cam.yfov,
                zfar: cam.zfar,
                znear: cam.znear,
                extensions: None,
                extras: Default::default(),
            }),
            type_: Valid(gj::camera::Type::Perspective),
            extensions: None,
            extras: Default::default(),
        });
        camera_index.insert(cam.id.to_string(), idx);
    }

    for light in &model.lights {
        let l = gj::extensions::scene::khr_lights_punctual::Light {
            color: {
                let c = linear(light.color);
                [c[0], c[1], c[2]]
            },
            extensions: None,
            extras: Default::default(),
            intensity: light.intensity,
            name: Some(light.name.clone()),
            range: light.range,
            spot: match light.kind {
                LightKind::Spot => Some(gj::extensions::scene::khr_lights_punctual::Spot {
                    inner_cone_angle: 0.0,
                    outer_cone_angle: std::f32::consts::FRAC_PI_4,
                }),
                _ => None,
            },
            type_: Valid(match light.kind {
                LightKind::Directional => gj::extensions::scene::khr_lights_punctual::Type::Directional,
                LightKind::Point => gj::extensions::scene::khr_lights_punctual::Type::Point,
                LightKind::Spot => gj::extensions::scene::khr_lights_punctual::Type::Spot,
            }),
        };
        let lights: &mut Vec<_> = cx.root.as_mut();
        lights.push(l);
        light_index.insert(light.id.to_string(), (lights.len() - 1) as u32);
    }
    if !model.lights.is_empty() {
        cx.root.extensions_used.push("KHR_lights_punctual".into());
    }

    // Nodes in document order, so indices are stable across exports.
    for node in &model.nodes {
        let mesh = match &node.mesh {
            Some(mesh_id) => {
                let key = (mesh_id.clone(), node.material.clone());
                Some(match mesh_index.get(&key) {
                    Some(i) => *i,
                    None => {
                        let data = build_mesh(project, doc, mesh_id, assets)?;
                        let tangents = node
                            .material
                            .as_ref()
                            .and_then(|m| needs_tangents.get(m))
                            .copied()
                            .unwrap_or(false);
                        let idx = export_mesh(
                            &mut cx,
                            &data,
                            node.material.as_ref().and_then(|m| material_index.get(m)).copied(),
                            tangents,
                            &model
                                .mesh(mesh_id)
                                .map(|m| m.name.clone())
                                .unwrap_or_else(|| mesh_id.to_string()),
                        )?;
                        mesh_index.insert(key, idx);
                        idx
                    }
                })
            }
            None => None,
        };
        let extensions = node.light.as_ref().and_then(|l| light_index.get(l.as_str())).map(|i| {
            gj::extensions::scene::Node {
                khr_lights_punctual: Some(
                    gj::extensions::scene::khr_lights_punctual::KhrLightsPunctual {
                        light: gj::Index::new(*i),
                    },
                ),
            }
        });
        let idx = cx.root.push(gj::Node {
            camera: node
                .camera
                .as_ref()
                .and_then(|c| camera_index.get(c.as_str()))
                .copied(),
            children: None,
            extensions,
            extras: Default::default(),
            matrix: None,
            mesh,
            name: Some(node.name.clone()),
            rotation: Some(gj::scene::UnitQuaternion(node.rotation)),
            scale: Some(node.scale),
            translation: Some(node.translation),
            skin: None,
            weights: None,
        });
        node_index.insert(node.id.clone(), idx);
    }
    for node in &model.nodes {
        let kids: Vec<gj::Index<gj::Node>> = node
            .children
            .iter()
            .filter_map(|c| node_index.get(c).copied())
            .collect();
        if !kids.is_empty() {
            let me = node_index[&node.id];
            cx.root.nodes[me.value()].children = Some(kids);
        }
    }

    let mut roots: Vec<gj::Index<gj::Node>> = model
        .roots()
        .iter()
        .filter_map(|n| node_index.get(&n.id).copied())
        .collect();
    if model.up_axis == UpAxis::Z && !roots.is_empty() {
        // glTF is Y-up: rotate the whole scene -90 degrees about X so authored +Z points up.
        let fix = cx.root.push(gj::Node {
            camera: None,
            children: Some(roots.clone()),
            extensions: None,
            extras: Default::default(),
            matrix: None,
            mesh: None,
            name: Some("z-up-correction".into()),
            rotation: Some(gj::scene::UnitQuaternion([
                -std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
            ])),
            scale: None,
            translation: None,
            skin: None,
            weights: None,
        });
        roots = vec![fix];
    }
    let scene = cx.root.push(gj::Scene {
        extensions: None,
        extras: Default::default(),
        name: Some(model.name.clone()),
        nodes: roots,
    });
    cx.root.scene = Some(scene);

    export_animations(&mut cx, model, &node_index)?;

    if cx.emissive_strength_used {
        cx.root
            .extensions_used
            .push("KHR_materials_emissive_strength".into());
    }

    cx.bin.align();
    let bin = std::mem::take(&mut cx.bin.data);
    cx.root.buffers[buffer.value()].byte_length = USize64::from(bin.len());
    let mut root = cx.root;
    if bin.is_empty() {
        root.buffers.clear();
    }
    Ok((root, bin, buffer))
}

/// 12-byte header, JSON chunk padded with spaces, BIN chunk padded with zeros.
pub fn pack_glb(json: &[u8], bin: &[u8]) -> Vec<u8> {
    let json_pad = (4 - json.len() % 4) % 4;
    let bin_pad = (4 - bin.len() % 4) % 4;
    let json_len = json.len() + json_pad;
    let bin_len = bin.len() + bin_pad;
    let total = 12 + 8 + json_len + if bin.is_empty() { 0 } else { 8 + bin_len };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_len as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(json);
    out.extend(std::iter::repeat(b' ').take(json_pad));
    if !bin.is_empty() {
        out.extend_from_slice(&(bin_len as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(bin);
        out.extend(std::iter::repeat(0u8).take(bin_pad));
    }
    debug_assert_eq!(out.len(), total);
    out
}

fn export_mesh(
    cx: &mut Ctx,
    data: &MeshData,
    material: Option<gj::Index<gj::Material>>,
    with_tangents: bool,
    name: &str,
) -> Result<gj::Index<gj::Mesh>> {
    if data.positions.is_empty() || data.indices.is_empty() {
        return Err(Error::DegenerateGeometry(format!(
            "mesh '{name}' has no triangles to export"
        )));
    }
    let flat: Vec<f32> = data.positions.iter().flat_map(|p| p.iter().copied()).collect();
    let pos = cx.f32_accessor(
        &flat,
        3,
        gj::accessor::Type::Vec3,
        true,
        Some(gj::buffer::Target::ArrayBuffer),
        &format!("{name}/POSITION"),
    );
    let mut attributes = BTreeMap::new();
    attributes.insert(Valid(gj::mesh::Semantic::Positions), pos);
    if data.normals.len() == data.positions.len() {
        let flat: Vec<f32> = data.normals.iter().flat_map(|p| p.iter().copied()).collect();
        let acc = cx.f32_accessor(
            &flat,
            3,
            gj::accessor::Type::Vec3,
            false,
            Some(gj::buffer::Target::ArrayBuffer),
            &format!("{name}/NORMAL"),
        );
        attributes.insert(Valid(gj::mesh::Semantic::Normals), acc);
    }
    if data.uvs.len() == data.positions.len() {
        let flat: Vec<f32> = data.uvs.iter().flat_map(|p| p.iter().copied()).collect();
        let acc = cx.f32_accessor(
            &flat,
            2,
            gj::accessor::Type::Vec2,
            false,
            Some(gj::buffer::Target::ArrayBuffer),
            &format!("{name}/TEXCOORD_0"),
        );
        attributes.insert(Valid(gj::mesh::Semantic::TexCoords(0)), acc);
    }
    if with_tangents && data.normals.len() == data.positions.len() {
        let tangents = crate::uv::tangents(data);
        if tangents.len() == data.positions.len() {
            let flat: Vec<f32> = tangents.iter().flat_map(|p| p.iter().copied()).collect();
            let acc = cx.f32_accessor(
                &flat,
                4,
                gj::accessor::Type::Vec4,
                false,
                Some(gj::buffer::Target::ArrayBuffer),
                &format!("{name}/TANGENT"),
            );
            attributes.insert(Valid(gj::mesh::Semantic::Tangents), acc);
        }
    }
    let (off, len) = cx.bin.push_u32(&data.indices);
    let view = cx.view(off, len, Some(gj::buffer::Target::ElementArrayBuffer));
    let indices = cx.accessor(
        view,
        data.indices.len(),
        gj::accessor::ComponentType::U32,
        gj::accessor::Type::Scalar,
        None,
        None,
        &format!("{name}/indices"),
    );
    let primitive = gj::mesh::Primitive {
        attributes,
        extensions: None,
        extras: Default::default(),
        indices: Some(indices),
        material,
        mode: Valid(gj::mesh::Mode::Triangles),
        targets: None,
    };
    Ok(cx.root.push(gj::Mesh {
        extensions: None,
        extras: Default::default(),
        name: Some(name.to_string()),
        primitives: vec![primitive],
        weights: None,
    }))
}

fn export_material(cx: &mut Ctx, m: &Material) -> Result<(gj::Index<gj::Material>, bool)> {
    let mut pbr = gj::material::PbrMetallicRoughness {
        base_color_factor: gj::material::PbrBaseColorFactor(linear(m.base_color)),
        base_color_texture: None,
        metallic_factor: gj::material::StrengthFactor(m.metallic.clamp(0.0, 1.0)),
        roughness_factor: gj::material::StrengthFactor(m.roughness.clamp(0.0, 1.0)),
        metallic_roughness_texture: None,
        extensions: None,
        extras: Default::default(),
    };
    let mut normal_texture = None;
    let mut occlusion_texture = None;
    let mut emissive_texture = None;
    for binding in &m.textures {
        let tex = cx.texture(&binding.source)?;
        let info = gj::texture::Info {
            index: tex,
            tex_coord: binding.uv_set,
            extensions: None,
            extras: Default::default(),
        };
        match binding.slot {
            TextureSlot::BaseColor => pbr.base_color_texture = Some(info),
            TextureSlot::MetallicRoughness => pbr.metallic_roughness_texture = Some(info),
            TextureSlot::Emissive => emissive_texture = Some(info),
            TextureSlot::Normal => {
                normal_texture = Some(gj::material::NormalTexture {
                    index: tex,
                    scale: binding.scale,
                    tex_coord: binding.uv_set,
                    extensions: None,
                    extras: Default::default(),
                })
            }
            TextureSlot::Occlusion => {
                occlusion_texture = Some(gj::material::OcclusionTexture {
                    index: tex,
                    strength: gj::material::StrengthFactor(binding.scale.clamp(0.0, 1.0)),
                    tex_coord: binding.uv_set,
                    extensions: None,
                    extras: Default::default(),
                })
            }
        }
    }
    let emissive = m.emissive.unwrap_or(Color::BLACK);
    let e = linear(emissive);
    // A zero strength with an emissive colour means "just use the colour": glTF's own
    // default strength is 1.0, and 0.0 would silently switch the emission off.
    let strength = if m.emissive_strength > 0.0 { m.emissive_strength } else { 1.0 };
    let extensions = if m.emissive.is_some() && (strength - 1.0).abs() > 1e-6 {
        cx.emissive_strength_used = true;
        Some(gj::extensions::material::Material {
            emissive_strength: Some(gj::extensions::material::EmissiveStrength {
                emissive_strength: gj::extensions::material::EmissiveStrengthFactor(strength),
            }),
            ..Default::default()
        })
    } else {
        None
    };
    let material = gj::Material {
        alpha_cutoff: None,
        alpha_mode: Valid(match m.alpha_mode {
            Some(AlphaMode::Mask) => gj::material::AlphaMode::Mask,
            Some(AlphaMode::Blend) => gj::material::AlphaMode::Blend,
            _ => gj::material::AlphaMode::Opaque,
        }),
        double_sided: m.double_sided,
        name: Some(m.name.clone()),
        pbr_metallic_roughness: pbr,
        normal_texture,
        occlusion_texture,
        emissive_texture,
        emissive_factor: gj::material::EmissiveFactor([e[0], e[1], e[2]]),
        extensions,
        extras: Default::default(),
    };
    let has_normal_map = material.normal_texture.is_some();
    Ok((cx.root.push(material), has_normal_map))
}

fn export_animations(
    cx: &mut Ctx,
    model: &ModelDoc,
    node_index: &BTreeMap<NodeId, gj::Index<gj::Node>>,
) -> Result<()> {
    for anim in &model.animations {
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        for ch in &anim.channels {
            let Some(node) = node_index.get(&ch.node).copied() else {
                return Err(Error::Invalid(format!(
                    "animation '{}' targets missing node '{}'",
                    anim.id, ch.node
                )));
            };
            if ch.keys.is_empty() {
                continue;
            }
            let comps = match ch.path {
                AnimPath::Rotation => 4,
                _ => 3,
            };
            let per_key = match ch.interpolation {
                Interpolation::CubicSpline => comps * 3,
                _ => comps,
            };
            let mut keys = ch.keys.clone();
            keys.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
            let mut times = Vec::with_capacity(keys.len());
            let mut values = Vec::with_capacity(keys.len() * per_key);
            for k in &keys {
                if k.v.len() != per_key {
                    return Err(Error::Invalid(format!(
                        "animation '{}' channel on {} expects {} values per key ({:?}), got {}",
                        anim.id,
                        ch.node,
                        per_key,
                        ch.interpolation,
                        k.v.len()
                    )));
                }
                times.push(k.t);
                values.extend_from_slice(&k.v);
            }
            let input = cx.f32_accessor(
                &times,
                1,
                gj::accessor::Type::Scalar,
                true,
                None,
                &format!("{}/input", anim.id),
            );
            let output = cx.f32_accessor(
                &values,
                comps,
                if comps == 4 { gj::accessor::Type::Vec4 } else { gj::accessor::Type::Vec3 },
                false,
                None,
                &format!("{}/output", anim.id),
            );
            let sampler_index = samplers.len() as u32;
            samplers.push(gj::animation::Sampler {
                extensions: None,
                extras: Default::default(),
                input,
                interpolation: Valid(match ch.interpolation {
                    Interpolation::Linear => gj::animation::Interpolation::Linear,
                    Interpolation::Step => gj::animation::Interpolation::Step,
                    Interpolation::CubicSpline => gj::animation::Interpolation::CubicSpline,
                }),
                output,
            });
            channels.push(gj::animation::Channel {
                sampler: gj::Index::new(sampler_index),
                target: gj::animation::Target {
                    extensions: None,
                    extras: Default::default(),
                    node,
                    path: Valid(match ch.path {
                        AnimPath::Translation => gj::animation::Property::Translation,
                        AnimPath::Rotation => gj::animation::Property::Rotation,
                        AnimPath::Scale => gj::animation::Property::Scale,
                    }),
                },
                extensions: None,
                extras: Default::default(),
            });
        }
        if channels.is_empty() {
            continue;
        }
        cx.root.push(gj::Animation {
            extensions: None,
            extras: Default::default(),
            channels,
            name: Some(if anim.name.is_empty() { anim.id.to_string() } else { anim.name.clone() }),
            samplers,
        });
    }
    Ok(())
}

/// Structural conformance of what this document would export, as gltf-json sees it.
pub fn structural_errors(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
    textures: &TextureResolver<'_>,
) -> Result<Vec<String>> {
    let (root, _bin, _buffer) = build_root(project, doc, assets, textures)?;
    let mut errors = Vec::new();
    {
        use gj::validation::Validate;
        root.validate(&root, gj::Path::new, &mut |path, err| {
            errors.push(format!("{}: {err:?}", path()));
        });
    }
    Ok(errors)
}

/// World-space bounds of every drawable in the scene.
pub fn scene_bounds(
    project: &Project,
    doc: &DocId,
    assets: &AssetStore,
) -> Result<Option<([f32; 3], [f32; 3])>> {
    let model = project.model(doc)?;
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let mut any = false;
    for (node_id, world) in world_transforms(model) {
        let Some(node) = model.node(&node_id) else { continue };
        let Some(mesh_id) = &node.mesh else { continue };
        let mut data = build_mesh(project, doc, mesh_id, assets)?;
        data.transform(&world);
        if let Some((l, h)) = data.bounds() {
            any = true;
            for i in 0..3 {
                lo[i] = lo[i].min(l[i]);
                hi[i] = hi[i].max(h[i]);
            }
        }
    }
    Ok(any.then_some((lo, hi)))
}
