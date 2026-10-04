use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameSpec {
    pub name: String,
    pub count: Option<usize>,
    pub bits: Option<u8>,
}

/// Parses `Name`, `Name[16]`, `Name:1` or `Name[2]:1`, tolerating surrounding spaces.
pub fn parse_name(raw: &str) -> Option<NameSpec> {
    let (head, bits) = match raw.split_once(':') {
        Some((h, b)) => (h, Some(b.trim().parse().ok()?)),
        None => (raw, None),
    };
    let (name, count) = match head.split_once('[') {
        Some((n, rest)) => (n, Some(rest.trim().strip_suffix(']')?.trim().parse().ok()?)),
        None => (head, None),
    };
    let name = name.trim();
    (!name.is_empty()).then(|| NameSpec { name: name.to_owned(), count, bits })
}

#[derive(Clone, Debug)]
pub struct DexField {
    pub ty: String,
    pub spec: NameSpec,
}

#[derive(Clone, Debug)]
pub struct DexDef {
    /// Paramdex game folder plus file, e.g. `ACFA/ArenaAcInfo.xml`.
    pub source: String,
    pub fields: Vec<DexField>,
}

/// Paramdex `Def` attribute: `type name[N]:bits = default`.
fn parse_dex_def(def: &str) -> Option<DexField> {
    let lhs = def.split('=').next()?.trim();
    let (ty, rest) = lhs.split_once(char::is_whitespace)?;
    Some(DexField { ty: ty.to_owned(), spec: parse_name(rest)? })
}

/// Appends every Paramdex XML for `game` in `dir`, keyed by ParamType (filenames are arbitrary upstream).
pub fn load_paramdex(out: &mut HashMap<String, Vec<DexDef>>, game: &str, dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("xml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let doc = roxmltree::Document::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        let child_text = |tag: &str| doc.descendants().find(|n| n.has_tag_name(tag)).and_then(|n| n.text()).map(str::trim);
        let Some(param_type) = child_text("ParamType") else { continue };
        let fields = doc
            .descendants()
            .filter(|n| n.has_tag_name("Field"))
            .filter_map(|n| n.attribute("Def").and_then(parse_dex_def))
            .collect();
        out.entry(param_type.to_owned()).or_default().push(DexDef {
            source: format!("{game}/{}", path.file_name().unwrap().to_string_lossy()),
            fields,
        });
    }
    Ok(())
}

const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate", "do", "dyn", "else", "enum",
    "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut",
    "override", "priv", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// `bObjBreakBulletEnable` -> `b_obj_break_bullet_enable`, `XMLData` -> `xml_data`.
pub fn snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            out.push('_');
            continue;
        }
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    let mut ident = out.split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
    if ident.is_empty() || ident.starts_with(|c: char| c.is_ascii_digit()) {
        ident.insert_str(0, "f_");
    }
    if KEYWORDS.contains(&ident.as_str()) {
        ident.push('_');
    }
    ident
}

/// `DESTROY_AP_ST` -> `DestroyApSt`.
pub fn pascal(s: &str) -> String {
    let mut out: String = snake(s)
        .split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'T');
    }
    out
}

/// `_unknown/param/100144036` -> `UNKNOWN_PARAM_100144036`.
pub fn shouty(s: &str) -> String {
    let mut out = snake(s).to_ascii_uppercase();
    if out.starts_with("F_") && s.starts_with(|c: char| c.is_ascii_digit()) {
        out = format!("T_{}", &out[2..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(snake("bObjBreakBulletEnable"), "b_obj_break_bullet_enable");
        assert_eq!(snake("AP"), "ap");
        assert_eq!(snake("XMLData"), "xml_data");
        assert_eq!(snake("type"), "type_");
        assert_eq!(pascal("DESTROY_AP_ST"), "DestroyApSt");
        assert_eq!(parse_name("ObjectName[16]").unwrap().count, Some(16));
        assert_eq!(parse_name("bFlag : 1").unwrap().bits, Some(1));
        assert_eq!(parse_dex_def("f32 CollisionDamageCoef = 1").unwrap().spec.name, "CollisionDamageCoef");
    }
}
