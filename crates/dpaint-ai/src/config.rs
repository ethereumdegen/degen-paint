//! Provider configuration.
//!
//! Model ids are configuration, never constants baked into an op: the provider catalogs move
//! faster than releases, and an agent must be able to point an op at any compatible endpoint.
//! Everything here has a working default, so a fresh install needs nothing but a key.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `~/.config/degen-paint/config.toml`
pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".config/degen-paint/config.toml"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct FalConfig {
    pub base_url: String,
    pub generate: String,
    pub edit: String,
    pub inpaint: String,
    pub outpaint: String,
    pub upscale: String,
    pub remove_bg: String,
    pub texture: String,
}

impl Default for FalConfig {
    fn default() -> Self {
        Self {
            base_url: "https://queue.fal.run".into(),
            generate: "fal-ai/flux/dev".into(),
            edit: "fal-ai/flux-pro/kontext".into(),
            inpaint: "fal-ai/flux-general/inpainting".into(),
            // fal has no separate outpaint endpoint: outpainting is inpainting of the
            // margin that was added to the canvas.
            outpaint: "fal-ai/flux-general/inpainting".into(),
            upscale: "fal-ai/clarity-upscaler".into(),
            remove_bg: "fal-ai/birefnet".into(),
            texture: "fal-ai/flux/dev".into(),
        }
    }
}

impl FalConfig {
    /// The configured model for an op id. Unknown ids fall back to the generate model.
    pub fn model_for(&self, op: &str) -> &str {
        match op {
            "ai.image.edit" => &self.edit,
            "ai.image.inpaint" => &self.inpaint,
            "ai.image.outpaint" => &self.outpaint,
            "ai.image.upscale" => &self.upscale,
            "ai.image.remove-background" => &self.remove_bg,
            "ai.texture.generate" => &self.texture,
            _ => &self.generate,
        }
    }

    pub fn models(&self) -> BTreeMap<String, String> {
        [
            ("generate", &self.generate),
            ("edit", &self.edit),
            ("inpaint", &self.inpaint),
            ("outpaint", &self.outpaint),
            ("upscale", &self.upscale),
            ("remove_bg", &self.remove_bg),
            ("texture", &self.texture),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct QuiverConfig {
    pub base_url: String,
    /// Default model for both generation and vectorization.
    pub model: String,
}

impl Default for QuiverConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.quiver.ai/v1".into(),
            model: "arrow-2".into(),
        }
    }
}

impl QuiverConfig {
    pub fn models(&self) -> BTreeMap<String, String> {
        [("default".to_string(), self.model.clone())].into_iter().collect()
    }
}

/// Estimated price per op, in USD. Used for the budget pre-check and for accounting when a
/// provider does not report a price with the result. Overridable in `[ai.cost]`.
pub fn default_costs() -> BTreeMap<String, f64> {
    [
        ("ai.image.generate", 0.025),
        ("ai.image.edit", 0.04),
        ("ai.image.inpaint", 0.04),
        ("ai.image.outpaint", 0.04),
        ("ai.image.upscale", 0.02),
        ("ai.image.remove-background", 0.01),
        ("ai.texture.generate", 0.025),
        ("ai.vector.generate", 0.06),
        ("ai.vector.vectorize", 0.03),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct AiConfig {
    pub fal: FalConfig,
    pub quiver: QuiverConfig,
    /// Delay between queue status polls. Tests set this to zero.
    pub poll_interval_ms: u64,
    pub poll_max_attempts: u32,
    pub timeout_secs: u64,
    /// Per-op estimated price in USD, keyed by op id.
    pub costs: BTreeMap<String, f64>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            fal: FalConfig::default(),
            quiver: QuiverConfig::default(),
            poll_interval_ms: 1000,
            poll_max_attempts: 600,
            timeout_secs: 120,
            costs: default_costs(),
        }
    }
}

impl AiConfig {
    /// Load `~/.config/degen-paint/config.toml` if it exists, else defaults. A malformed file
    /// is not fatal: the defaults still produce a working tool.
    pub fn load() -> Self {
        match default_config_path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        let Some(file) = ConfigFile::read(path) else {
            return Self::default();
        };
        let mut cfg = Self::default();
        let Some(ai) = file.ai else { return cfg };
        if let Some(f) = ai.fal {
            f.apply(&mut cfg.fal);
        }
        if let Some(q) = ai.quiver {
            if let Some(v) = q.base_url {
                cfg.quiver.base_url = v;
            }
            if let Some(v) = q.model {
                cfg.quiver.model = v;
            }
        }
        if let Some(v) = ai.poll_interval_ms {
            cfg.poll_interval_ms = v;
        }
        if let Some(v) = ai.poll_max_attempts {
            cfg.poll_max_attempts = v;
        }
        if let Some(v) = ai.timeout_secs {
            cfg.timeout_secs = v;
        }
        for (k, v) in ai.cost {
            cfg.costs.insert(k, v);
        }
        cfg
    }

    /// Estimated price of one call of `op`, in USD.
    pub fn cost_of(&self, op: &str) -> f64 {
        self.costs.get(op).copied().unwrap_or(0.0)
    }
}

/// The on-disk `config.toml`, which also carries optional keys.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ConfigFile {
    pub ai: Option<AiFile>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct AiFile {
    pub fal: Option<FalFile>,
    pub quiver: Option<QuiverFile>,
    pub poll_interval_ms: Option<u64>,
    pub poll_max_attempts: Option<u32>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub cost: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct FalFile {
    pub key: Option<String>,
    pub base_url: Option<String>,
    pub generate: Option<String>,
    pub edit: Option<String>,
    pub inpaint: Option<String>,
    pub outpaint: Option<String>,
    pub upscale: Option<String>,
    pub remove_bg: Option<String>,
    pub texture: Option<String>,
}

impl FalFile {
    fn apply(self, cfg: &mut FalConfig) {
        macro_rules! set {
            ($($field:ident),*) => { $(if let Some(v) = self.$field { cfg.$field = v; })* };
        }
        set!(base_url, generate, edit, inpaint, outpaint, upscale, remove_bg, texture);
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct QuiverFile {
    pub key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl ConfigFile {
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_come_from_the_file_not_from_constants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[ai.fal]
generate = "fal-ai/recraft-v3"
upscale  = "fal-ai/esrgan"

[ai.quiver]
model = "arrow-2-telos"

[ai.cost]
"ai.image.generate" = 0.5
"#,
        )
        .unwrap();

        let cfg = AiConfig::load_from(&path);
        assert_eq!(cfg.fal.model_for("ai.image.generate"), "fal-ai/recraft-v3");
        assert_eq!(cfg.fal.model_for("ai.image.upscale"), "fal-ai/esrgan");
        // Untouched entries keep their documented defaults.
        assert_eq!(cfg.fal.model_for("ai.image.inpaint"), "fal-ai/flux-general/inpainting");
        assert_eq!(cfg.quiver.model, "arrow-2-telos");
        assert_eq!(cfg.cost_of("ai.image.generate"), 0.5);
        assert_eq!(cfg.cost_of("ai.image.upscale"), 0.02);
    }

    #[test]
    fn a_missing_or_broken_config_still_yields_a_working_tool() {
        let dir = tempfile::tempdir().unwrap();
        let missing = AiConfig::load_from(&dir.path().join("nope.toml"));
        assert_eq!(missing.fal.model_for("ai.image.generate"), "fal-ai/flux/dev");

        let broken = dir.path().join("broken.toml");
        std::fs::write(&broken, "this is not toml {{{").unwrap();
        assert_eq!(AiConfig::load_from(&broken), AiConfig::default());
    }
}
