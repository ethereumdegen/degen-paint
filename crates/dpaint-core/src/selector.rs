//! Selectors. Ops address objects by stable id, name, type and attribute — never by index,
//! because an index shifts the moment anything is reordered. See `docs/selectors.md`.

use crate::doc::{Document, DocKind};
use crate::error::{Error, Result};
use crate::ids::DocId;
use crate::project::Project;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Optional `doc:` prefix.
    pub document: Option<String>,
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub source: Source,
    pub predicates: Vec<Predicate>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Id(String),
    Name(String),
    Type(String),
    All,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Attr { key: String, op: AttrOp, value: String },
    Pseudo(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp {
    Eq,
    Ne,
    Prefix,
    Suffix,
    Contains,
    Gt,
    Lt,
}

/// One resolved object, flattened so callers do not need to know the document shape.
#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub document: DocId,
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub depth: usize,
}

/// Closed set of pseudo-classes, so `doc:` prefixes stay unambiguous.
pub const PSEUDOS: &[&str] = &[
    "first", "last", "visible", "hidden", "locked", "empty", "selected",
];

impl Selector {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(Error::SelectorSyntax("empty selector".into()));
        }
        // A `doc:` prefix only counts when the part after the colon is not a pseudo:
        // `*:hidden` is a pseudo on every object, `logo:#mark` is a cross-document target.
        let (document, rest) = match input.split_once(':') {
            Some((d, r))
                if !d.is_empty()
                    && !r.is_empty()
                    && !d.starts_with('#')
                    && !d.starts_with('@')
                    && d != "*"
                    && !d.contains('[')
                    && !d.contains(' ')
                    && !PSEUDOS.contains(&r.split([':', '[']).next().unwrap_or("")) =>
            {
                (Some(d.to_string()), r)
            }
            _ => (None, input),
        };

        let mut terms = Vec::new();
        for part in rest.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            terms.push(Term::parse(part)?);
        }
        if terms.is_empty() {
            return Err(Error::SelectorSyntax(format!("no terms in '{input}'")));
        }
        Ok(Selector { document, terms })
    }
}

impl Term {
    fn parse(s: &str) -> Result<Self> {
        let head_end = s.find(['[', ':']).unwrap_or(s.len());
        let head = &s[..head_end];
        let tail = &s[head_end..];

        let source = match head.chars().next() {
            Some('#') => Source::Id(head[1..].to_string()),
            Some('@') => Source::Name(head[1..].to_string()),
            Some('*') | None => Source::All,
            Some(_) => Source::Type(head.to_string()),
        };

        let mut predicates = Vec::new();
        let mut rest = tail;
        while !rest.is_empty() {
            if let Some(stripped) = rest.strip_prefix('[') {
                let end = stripped
                    .find(']')
                    .ok_or_else(|| Error::SelectorSyntax(format!("unclosed '[' in '{s}'")))?;
                predicates.push(Predicate::parse_attr(&stripped[..end])?);
                rest = &stripped[end + 1..];
            } else if let Some(stripped) = rest.strip_prefix(':') {
                let end = stripped.find('[').unwrap_or(stripped.len());
                let p = &stripped[..end];
                if p.is_empty() {
                    return Err(Error::SelectorSyntax(format!("empty pseudo in '{s}'")));
                }
                predicates.push(Predicate::Pseudo(p.to_string()));
                rest = &stripped[end..];
            } else {
                return Err(Error::SelectorSyntax(format!("unexpected '{rest}' in '{s}'")));
            }
        }
        Ok(Term { source, predicates })
    }
}

impl Predicate {
    fn parse_attr(body: &str) -> Result<Self> {
        for (tok, op) in [
            ("!=", AttrOp::Ne),
            ("^=", AttrOp::Prefix),
            ("$=", AttrOp::Suffix),
            ("*=", AttrOp::Contains),
            (">", AttrOp::Gt),
            ("<", AttrOp::Lt),
            ("=", AttrOp::Eq),
        ] {
            if let Some((k, v)) = body.split_once(tok) {
                return Ok(Predicate::Attr {
                    key: k.trim().to_string(),
                    op,
                    value: v.trim().trim_matches(['"', '\'']).to_string(),
                });
            }
        }
        Err(Error::SelectorSyntax(format!("bad attribute predicate '[{body}]'")))
    }
}

/// A candidate object, as a flat record the matcher can test without knowing document shapes.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub type_name: String,
    pub depth: usize,
    pub attrs: serde_json::Value,
}

/// Flatten a document into candidates in document order (z-order for layers).
pub fn candidates(doc: &Document) -> Vec<Candidate> {
    let mut out = Vec::new();
    match doc {
        Document::Raster(d) => {
            fn rec(ls: &[crate::doc::raster::Layer], depth: usize, out: &mut Vec<Candidate>) {
                for l in ls {
                    out.push(Candidate {
                        id: l.id.to_string(),
                        name: l.name.clone(),
                        type_name: l.type_name().to_string(),
                        depth,
                        attrs: serde_json::to_value(l).unwrap_or(serde_json::Value::Null),
                    });
                    if let crate::doc::raster::LayerKind::Group { layers } = &l.kind {
                        rec(layers, depth + 1, out);
                    }
                }
            }
            rec(&d.layers, 0, &mut out);
        }
        Document::Vector(d) => {
            fn rec(os: &[crate::doc::vector::VObject], depth: usize, out: &mut Vec<Candidate>) {
                for o in os {
                    out.push(Candidate {
                        id: o.id.to_string(),
                        name: o.name.clone(),
                        type_name: o.type_name().to_string(),
                        depth,
                        attrs: serde_json::to_value(o).unwrap_or(serde_json::Value::Null),
                    });
                    if let crate::doc::vector::VKind::Group { objects } = &o.kind {
                        rec(objects, depth + 1, out);
                    }
                }
            }
            rec(&d.objects, 0, &mut out);
            for a in &d.artboards {
                out.push(Candidate {
                    id: a.id.to_string(),
                    name: a.name.clone(),
                    type_name: "artboard".into(),
                    depth: 0,
                    attrs: serde_json::to_value(a).unwrap_or(serde_json::Value::Null),
                });
            }
        }
        Document::Model(d) => {
            for n in &d.nodes {
                out.push(Candidate {
                    id: n.id.to_string(),
                    name: n.name.clone(),
                    type_name: "node".into(),
                    depth: 0,
                    attrs: serde_json::to_value(n).unwrap_or(serde_json::Value::Null),
                });
            }
            for m in &d.meshes {
                out.push(Candidate {
                    id: m.id.to_string(),
                    name: m.name.clone(),
                    type_name: "mesh".into(),
                    depth: 0,
                    attrs: serde_json::to_value(m).unwrap_or(serde_json::Value::Null),
                });
            }
            for m in &d.materials {
                out.push(Candidate {
                    id: m.id.to_string(),
                    name: m.name.clone(),
                    type_name: "material".into(),
                    depth: 0,
                    attrs: serde_json::to_value(m).unwrap_or(serde_json::Value::Null),
                });
            }
            for l in &d.lights {
                out.push(Candidate {
                    id: l.id.to_string(),
                    name: l.name.clone(),
                    type_name: "light".into(),
                    depth: 0,
                    attrs: serde_json::to_value(l).unwrap_or(serde_json::Value::Null),
                });
            }
            for c in &d.cameras {
                out.push(Candidate {
                    id: c.id.to_string(),
                    name: c.name.clone(),
                    type_name: "camera".into(),
                    depth: 0,
                    attrs: serde_json::to_value(c).unwrap_or(serde_json::Value::Null),
                });
            }
        }
    }
    out
}

fn attr_str(attrs: &serde_json::Value, key: &str) -> Option<String> {
    let v = attrs.get(key)?;
    Some(match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string().trim_matches('"').to_string(),
    })
}

fn matches(c: &Candidate, t: &Term, index: usize, total: usize) -> bool {
    let source_ok = match &t.source {
        Source::All => true,
        Source::Id(id) => c.id == *id,
        Source::Name(n) => c.name == *n,
        Source::Type(ty) => c.type_name == *ty,
    };
    if !source_ok {
        return false;
    }
    t.predicates.iter().all(|p| match p {
        Predicate::Attr { key, op, value } => {
            let actual = match key.as_str() {
                "id" => Some(c.id.clone()),
                "name" => Some(c.name.clone()),
                "type" => Some(c.type_name.clone()),
                k => attr_str(&c.attrs, k),
            };
            let Some(actual) = actual else { return false };
            match op {
                AttrOp::Eq => actual == *value,
                AttrOp::Ne => actual != *value,
                AttrOp::Prefix => actual.starts_with(value.as_str()),
                AttrOp::Suffix => actual.ends_with(value.as_str()),
                AttrOp::Contains => actual.contains(value.as_str()),
                AttrOp::Gt => cmp_num(&actual, value).map(|o| o.is_gt()).unwrap_or(false),
                AttrOp::Lt => cmp_num(&actual, value).map(|o| o.is_lt()).unwrap_or(false),
            }
        }
        Predicate::Pseudo(p) => match p.as_str() {
            "first" => index == 0,
            "last" => index + 1 == total,
            "visible" => c.attrs.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
            "hidden" => !c.attrs.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
            "locked" => c.attrs.get("locked").and_then(|v| v.as_bool()).unwrap_or(false),
            "empty" => c.attrs.get("layers").map(|l| l.as_array().map(|a| a.is_empty()).unwrap_or(false)).unwrap_or(false),
            _ => false,
        },
    })
}

fn cmp_num(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    a.parse::<f64>().ok()?.partial_cmp(&b.parse::<f64>().ok()?)
}

/// Resolve a selector against the project. Zero matches is an error carrying the real
/// candidate list, so a typo costs one turn instead of two.
pub fn resolve(project: &Project, selector: &str, default_doc: Option<&DocId>) -> Result<Vec<Match>> {
    let sel = Selector::parse(selector)?;
    let doc_id = match (&sel.document, default_doc) {
        (Some(d), _) => project.resolve_doc(Some(d))?,
        (None, Some(d)) => d.clone(),
        (None, None) => project.active.clone(),
    };
    let doc = project.doc(&doc_id)?;
    let cands = candidates(doc);
    let total = cands.len();

    let mut out: Vec<Match> = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        if sel.terms.iter().any(|t| matches(c, t, i, total)) {
            out.push(Match {
                document: doc_id.clone(),
                id: c.id.clone(),
                name: c.name.clone(),
                type_name: c.type_name.clone(),
                depth: c.depth,
            });
        }
    }

    if out.is_empty() {
        return Err(Error::SelectorNoMatch {
            selector: selector.to_string(),
            doc: doc_id.to_string(),
            candidates: cands.iter().map(|c| format!("#{}", c.id)).collect(),
        });
    }
    Ok(out)
}

/// Resolve a selector that must name exactly one object.
pub fn resolve_one(project: &Project, selector: &str, default_doc: Option<&DocId>) -> Result<Match> {
    let mut m = resolve(project, selector, default_doc)?;
    if m.len() > 1 {
        return Err(Error::SelectorAmbiguous {
            selector: selector.to_string(),
            count: m.len(),
            matches: m.iter().map(|x| format!("#{}", x.id)).collect(),
        });
    }
    Ok(m.remove(0))
}

/// Which document kinds a selector can apply to, used for early validation.
pub fn kind_of(project: &Project, sel: &Selector, default_doc: Option<&DocId>) -> Result<DocKind> {
    let doc_id = match (&sel.document, default_doc) {
        (Some(d), _) => project.resolve_doc(Some(d))?,
        (None, Some(d)) => d.clone(),
        (None, None) => project.active.clone(),
    };
    Ok(project.doc(&doc_id)?.kind())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;
    use crate::doc::{raster::*, vector::*, Document, Paint, RasterDoc, VectorDoc};
    use crate::ids::{LayerId, ObjectId};

    fn project() -> Project {
        let mut r = RasterDoc::new(DocId::from("doc_main"), "main", 100, 100);
        r.layers.push(Layer::new(LayerId::from("lyr_bg"), "bg", LayerKind::Fill { color: Color::WHITE }));
        let mut title = Layer::new(
            LayerId::from("lyr_title"),
            "title",
            LayerKind::Text {
                spec: crate::doc::TextSpec::new("hi"),
                fill: Paint::solid(Color::BLACK),
                stroke: None,
            },
        );
        title.opacity = 0.5;
        title.visible = false;
        r.layers.push(title);
        r.layers.push(Layer::new(
            LayerId::from("grp_fg"),
            "fg",
            LayerKind::Group {
                layers: vec![Layer::new(
                    LayerId::from("lyr_inner"),
                    "inner",
                    LayerKind::Fill { color: Color::BLACK },
                )],
            },
        ));

        let mut v = VectorDoc::new(DocId::from("doc_logo"), "logo", 64.0, 64.0);
        v.objects.push(VObject::new(
            ObjectId::from("obj_mark"),
            "mark",
            VKind::Path { d: "M0 0 H10".into() },
        ));

        let mut p = Project::new("t", Document::Raster(r));
        p.add_document(Document::Vector(v));
        p
    }

    #[test]
    fn parses_ids_names_types_predicates_and_document_prefixes() {
        assert_eq!(
            Selector::parse("#lyr_sky").unwrap().terms[0].source,
            Source::Id("lyr_sky".into())
        );
        assert_eq!(Selector::parse("@sky").unwrap().terms[0].source, Source::Name("sky".into()));
        assert_eq!(Selector::parse("*").unwrap().terms[0].source, Source::All);

        let s = Selector::parse("logo:#obj_mark").unwrap();
        assert_eq!(s.document.as_deref(), Some("logo"));

        let s = Selector::parse("layer[type=text][opacity<1]:hidden").unwrap();
        assert_eq!(s.terms[0].predicates.len(), 3);
        assert!(Selector::parse("layer[bad").is_err());
    }

    #[test]
    fn resolves_nested_layers_in_z_order() {
        let p = project();
        let all = resolve(&p, "*", None).unwrap();
        assert_eq!(
            all.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["lyr_bg", "lyr_title", "grp_fg", "lyr_inner"]
        );
        assert_eq!(all[3].depth, 1);
    }

    #[test]
    fn filters_by_type_attribute_and_pseudo() {
        let p = project();
        assert_eq!(resolve(&p, "text", None).unwrap().len(), 1);
        assert_eq!(resolve(&p, "*[opacity<1]", None).unwrap()[0].id, "lyr_title");
        assert_eq!(resolve(&p, "*:hidden", None).unwrap()[0].id, "lyr_title");
        assert_eq!(resolve(&p, "*:first", None).unwrap()[0].id, "lyr_bg");
        assert_eq!(resolve(&p, "*[name^=in]", None).unwrap()[0].id, "lyr_inner");
    }

    #[test]
    fn a_union_selects_both_sides_once_each() {
        let p = project();
        let m = resolve(&p, "#lyr_bg, #lyr_inner", None).unwrap();
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn cross_document_prefix_targets_the_other_document() {
        let p = project();
        let m = resolve(&p, "logo:#obj_mark", None).unwrap();
        assert_eq!(m[0].document.as_str(), "doc_logo");
        assert_eq!(m[0].type_name, "path");
    }

    #[test]
    fn zero_matches_errors_with_the_real_candidate_list() {
        let p = project();
        let err = resolve(&p, "#lyr_ttle", None).unwrap_err();
        assert_eq!(err.code(), "selector_no_match");
        let d = err.detail();
        assert!(d.candidates.contains(&"#lyr_title".to_string()));
        assert_eq!(d.suggestion.as_deref(), Some("#lyr_title"));
    }

    #[test]
    fn resolve_one_rejects_ambiguity_instead_of_picking() {
        let p = project();
        let err = resolve_one(&p, "fill", None).unwrap_err();
        assert_eq!(err.code(), "selector_ambiguous");
        assert_eq!(resolve_one(&p, "#lyr_bg", None).unwrap().id, "lyr_bg");
    }
}
