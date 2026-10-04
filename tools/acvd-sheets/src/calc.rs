//! `sheets/ac_ctrl_calc.csv` expressions: f32 Rust expressions whose names are checked against
//! the sheets and rewritten into generated code. A name is an earlier row; `<column>.<field>` is
//! an `ac_part_fields.csv` field of the part in that `design_parts.csv` column (0 when empty);
//! `<tuning file>.<ident>` is a `tuning_fields.csv` field. Integer literals become f32.

use anyhow::Result;

use crate::model::*;
use crate::names::{shouty, snake};

/// f32 methods an expression may call.
const METHODS: &[&str] = &["max", "min", "abs", "to_radians"];
const KEYWORDS: &[&str] = &["if", "else"];

pub struct Scope {
    columns: Vec<DesignPartRow>,
    fields: Vec<AcPartFieldRow>,
    /// `(prefix, ident, generated static)` per named tuning field.
    tuning: Vec<(String, String, String)>,
    /// Names of the rows evaluated so far.
    pub defined: Vec<String>,
}

impl Scope {
    pub fn load(paths: &Paths) -> Result<Self> {
        let sheet: TuningSheet = read_json(&paths.tuning())?;
        let named: Vec<TuningFieldRow> = read_csv(&paths.sheets().join("tuning_fields.csv"))?;
        let tuning = named
            .iter()
            .filter_map(|n| sheet.files.iter().find(|f| f.bin == n.file).map(|f| (snake(&f.name), n.ident.clone(), shouty(&f.name))))
            .collect();
        Ok(Self {
            columns: read_csv(&paths.sheets().join("design_parts.csv"))?,
            fields: read_csv(&paths.sheets().join("ac_part_fields.csv"))?,
            tuning,
            defined: Vec::new(),
        })
    }

    pub fn columns(&self) -> &[DesignPartRow] {
        &self.columns
    }

    fn part(&self, column: &str, field: &str) -> Result<String, String> {
        let c = self.columns.iter().find(|c| c.column == column).ok_or_else(|| format!("`{column}` is not a design_parts.csv column"))?;
        if !self.fields.iter().any(|f| f.category == c.category && f.name == field) {
            return Err(format!("category {} has no ac_part_fields.csv field `{field}`", c.category));
        }
        Ok(format!("crate::part_field(d.{column} as i64, {}, {field:?})", c.category))
    }

    fn is_prefix(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c.column == name) || self.tuning.iter().any(|(p, _, _)| p == name)
    }

    /// `expr` as generated Rust, or why it names something the sheets do not define.
    pub fn translate(&self, expr: &str) -> Result<String, String> {
        let tokens = tokenize(expr)?;
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let t = tokens[i].as_str();
            let ident = t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
            let qualified = ident && tokens.get(i + 1).is_some_and(|d| d == ".") && self.is_prefix(t);
            if qualified {
                let field = tokens.get(i + 2).ok_or_else(|| format!("`{t}.` lacks a field"))?;
                let code = match self.tuning.iter().find(|(p, f, _)| p == t && f == field) {
                    Some((_, f, s)) => format!("(crate::generated::tuning::{s}.{f} as f32)"),
                    None if self.tuning.iter().any(|(p, _, _)| p == t) => return Err(format!("tuning_fields.csv names no `{field}` in `{t}`")),
                    None => self.part(t, field)?,
                };
                out.push(code);
                i += 3;
                continue;
            }
            if ident {
                let method = out.last().is_some_and(|p| p == ".");
                let ok = if method { METHODS.contains(&t) } else { KEYWORDS.contains(&t) || self.defined.iter().any(|d| d == t) };
                if !ok {
                    return Err(if method { format!("`.{t}` is not one of {METHODS:?}") } else { format!("`{t}` is not an earlier row") });
                }
                out.push(t.to_owned());
            } else if t.starts_with(|c: char| c.is_ascii_digit()) {
                out.push(if t.contains('.') { t.to_owned() } else { format!("{t}.0") });
            } else {
                out.push(t.to_owned());
            }
            i += 1;
        }
        Ok(out.join(" ").replace(" . ", ".").replace(" (", "(").replace("( ", "(").replace(" )", ")"))
    }
}

fn tokenize(expr: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        } else if c.is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
        } else if ["==", "!=", "<=", ">=", "&&", "||"].iter().any(|op| expr[byte(&chars, i)..].starts_with(op)) {
            i += 2;
        } else if "+-*/(){}<>,.!".contains(c) {
            i += 1;
        } else {
            return Err(format!("unexpected `{c}`"));
        }
        out.push(chars[start..i].iter().collect());
    }
    Ok(out)
}

fn byte(chars: &[char], i: usize) -> usize {
    chars[..i].iter().map(|c| c.len_utf8()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        Scope {
            columns: vec![DesignPartRow { slot: 3, table: "T".into(), column: "legs".into(), category: 3, evidence: String::new() }],
            fields: vec![AcPartFieldRow { category: 3, offset: 8, ty: "u16".into(), name: "weight".into(), evidence: String::new() }],
            tuning: vec![("new_ac_behavior".into(), "gravity".into(), "NEW_AC_BEHAVIOR".into())],
            defined: vec!["total".into()],
        }
    }

    #[test]
    fn translates_names() {
        let s = scope();
        assert_eq!(
            s.translate("(total - legs.weight / 10).max(new_ac_behavior.gravity)").unwrap(),
            "(total - crate::part_field(d.legs as i64, 3, \"weight\") / 10.0).max((crate::generated::tuning::NEW_AC_BEHAVIOR.gravity as f32))"
        );
        assert_eq!(s.translate("if total < 1 { 0.5 } else { 2 }").unwrap(), "if total < 1.0 { 0.5 } else { 2.0 }");
        assert_eq!(s.translate("legs.weight == 3").unwrap(), "crate::part_field(d.legs as i64, 3, \"weight\") == 3.0");
    }

    #[test]
    fn rejects_unknown_names() {
        let s = scope();
        assert!(s.translate("later + 1").is_err());
        assert!(s.translate("legs.payload").is_err());
        assert!(s.translate("new_ac_behavior.walk").is_err());
        assert!(s.translate("total.sqrt()").is_err());
    }
}
