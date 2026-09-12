use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// USD per 1M tokens.
#[derive(Debug, Clone, Copy)]
pub struct Price {
    /// Uncached input tokens.
    pub input: f64,
    /// Cache-hit input tokens.
    pub cached: f64,
    /// Output/completion tokens.
    pub output: f64,
}

#[derive(Debug, Clone)]
pub enum Pricing {
    /// Billed at `price`.
    Paid,
    /// Free in Devin CLI — `price` is the equivalent list price of `billed_as`,
    /// shown struck through; actual charge is $0.
    Free { billed_as: String },
    /// No pricing rule matched; shown as "?".
    Unpriced,
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// Lowercase-normalized substring matched against the normalized model name.
    pub pattern: String,
    pub label: String,
    pub free: bool,
    /// Human label for the model the price is borrowed from (free models only).
    pub billed_as: Option<String>,
    pub price: Option<Price>,
}

pub struct Resolved {
    pub label: String,
    pub pricing: Pricing,
    pub price: Option<Price>,
}

pub struct PriceBook {
    rules: Vec<Rule>,
}

/// Normalize a raw model name for matching: lowercase, runs of
/// non-alphanumerics become a single '-'.
/// "SWE-1.7 Max" -> "swe-1-7-max", "GPT-5.6 Sol High Thinking" -> "gpt-5-6-sol-high-thinking"
pub fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true; // trim leading
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

impl PriceBook {
    pub fn new(extra: Vec<Rule>) -> Self {
        let mut rules = extra;
        rules.extend(default_rules());
        Self { rules }
    }

    pub fn resolve(&self, raw_model: &str) -> Resolved {
        let norm = normalize(raw_model);
        for r in &self.rules {
            if norm.contains(&r.pattern) {
                return Resolved {
                    label: r.label.clone(),
                    pricing: if r.free {
                        Pricing::Free {
                            billed_as: r.billed_as.clone().unwrap_or_else(|| r.pattern.clone()),
                        }
                    } else {
                        Pricing::Paid
                    },
                    price: r.price,
                };
            }
        }
        Resolved {
            label: raw_model.trim().to_string(),
            pricing: Pricing::Unpriced,
            price: None,
        }
    }
}

/// Rules are matched in order — put specific patterns before generic ones.
/// Prices are public API list prices (USD / 1M tokens), Sep 2026.
///
/// Cognition's SWE models are free inside Devin CLI but are priced here at
/// the Moonshot models they are based on (SWE-1.7 -> kimi-k2.7-code,
/// SWE-2 -> kimi-k3) so the "what would this have cost" figure is meaningful.
fn default_rules() -> Vec<Rule> {
    let r = |pattern: &str,
             label: &str,
             free: bool,
             billed_as: Option<&str>,
             i: f64,
             c: f64,
             o: f64| Rule {
        pattern: normalize(pattern),
        label: label.to_string(),
        free,
        billed_as: billed_as.map(|s| s.to_string()),
        price: Some(Price {
            input: i,
            cached: c,
            output: o,
        }),
    };
    vec![
        // --- Cognition (free in Devin CLI), priced at equivalent list rates ---
        r(
            "swe-1-7",
            "SWE-1.7",
            true,
            Some("kimi-k2.7-code"),
            0.95,
            0.19,
            4.00,
        ),
        r(
            "swe-1-6",
            "SWE-1.6",
            true,
            Some("kimi-k2.6"),
            0.95,
            0.16,
            4.00,
        ),
        r("swe-2", "SWE-2", true, Some("kimi-k3"), 3.00, 0.30, 15.00),
        r(
            "adaptive",
            "Adaptive",
            true,
            Some("kimi-k3 est."),
            3.00,
            0.30,
            15.00,
        ),
        r(
            "fusion",
            "Fusion",
            true,
            Some("fable-5.1 est."),
            10.00,
            0.25,
            50.00,
        ),
        // --- OpenAI ---
        r(
            "gpt-6-astra",
            "GPT-6 Astra",
            false,
            None,
            10.00,
            1.00,
            50.00,
        ),
        r("gpt-5-6-sol", "GPT-5.6 Sol", false, None, 4.00, 0.40, 20.00),
        r(
            "gpt-5-6-terra",
            "GPT-5.6 Terra",
            false,
            None,
            2.00,
            0.20,
            12.00,
        ),
        r(
            "gpt-5-6-luna",
            "GPT-5.6 Luna",
            false,
            None,
            0.20,
            0.02,
            1.20,
        ),
        r("gpt-5-6", "GPT-5.6", false, None, 4.00, 0.40, 20.00),
        r("gpt-5", "GPT-5", false, None, 2.00, 0.20, 12.00),
        // --- Anthropic ---
        r(
            "claude-fable",
            "Claude Fable",
            false,
            None,
            10.00,
            0.25,
            50.00,
        ),
        r(
            "claude-opus-5",
            "Claude Opus 5",
            false,
            None,
            5.00,
            0.50,
            25.00,
        ),
        r("claude-opus", "Claude Opus", false, None, 5.00, 0.50, 25.00),
        r(
            "claude-sonnet-5",
            "Claude Sonnet 5",
            false,
            None,
            2.00,
            0.20,
            10.00,
        ),
        r(
            "claude-sonnet",
            "Claude Sonnet",
            false,
            None,
            3.00,
            0.30,
            15.00,
        ),
        r(
            "claude-haiku",
            "Claude Haiku",
            false,
            None,
            1.00,
            0.10,
            5.00,
        ),
        // --- Google ---
        r(
            "gemini-3-pro",
            "Gemini 3 Pro",
            false,
            None,
            2.00,
            0.20,
            12.00,
        ),
        r(
            "gemini-3-5-flash",
            "Gemini 3.5 Flash",
            false,
            None,
            1.50,
            0.15,
            7.50,
        ),
        r(
            "gemini-3-flash",
            "Gemini 3 Flash",
            false,
            None,
            1.50,
            0.15,
            7.50,
        ),
        // --- Z.ai ---
        r("glm-5", "GLM-5", false, None, 1.40, 0.26, 4.40),
        // --- Moonshot (direct usage) ---
        r("kimi-k3", "Kimi K3", false, None, 3.00, 0.30, 15.00),
        r("kimi-k2-7", "Kimi K2.7 Code", false, None, 0.95, 0.19, 4.00),
        r("kimi-k2-6", "Kimi K2.6", false, None, 0.95, 0.16, 4.00),
        r("kimi", "Kimi", false, None, 0.60, 0.10, 2.50),
        // --- DeepSeek ---
        r("deepseek", "DeepSeek", false, None, 0.28, 0.03, 0.42),
    ]
}

#[derive(Deserialize)]
struct RuleFile {
    rule: Vec<RuleToml>,
}

#[derive(Deserialize)]
struct RuleToml {
    pattern: String,
    label: Option<String>,
    #[serde(default)]
    free: bool,
    billed_as: Option<String>,
    /// USD per 1M uncached input tokens.
    input: Option<f64>,
    /// USD per 1M cached input tokens (defaults to `input`).
    cached: Option<f64>,
    /// USD per 1M output tokens.
    output: Option<f64>,
}

/// Load extra rules from a TOML file; they take precedence over built-ins.
pub fn load_rules(path: &Path) -> Result<Vec<Rule>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read pricing file {}", path.display()))?;
    let file: RuleFile =
        toml::from_str(&text).with_context(|| format!("invalid TOML in {}", path.display()))?;
    Ok(file
        .rule
        .into_iter()
        .map(|r| Rule {
            pattern: normalize(&r.pattern),
            label: r.label.unwrap_or_else(|| r.pattern.clone()),
            free: r.free,
            billed_as: r.billed_as,
            price: r.input.map(|input| Price {
                input,
                cached: r.cached.unwrap_or(input),
                output: r.output.unwrap_or(0.0),
            }),
        })
        .collect())
}

pub fn default_config_path() -> std::path::PathBuf {
    std::env::home_dir()
        .unwrap_or_default()
        .join(".config/devin-tokenviz.toml")
}

/// Load `~/.config/devin-tokenviz.toml` if it exists.
pub fn load_default_rules() -> Vec<Rule> {
    let p = default_config_path();
    if p.exists() {
        load_rules(&p).unwrap_or_default()
    } else {
        Vec::new()
    }
}
