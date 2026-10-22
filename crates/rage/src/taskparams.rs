//! GTA IV's Euphoria tuning, `data/taskparams.txt` inside `pc/data/game.rpf` (the RPF's only file;
//! its contents are AES-encrypted like the TOC). Rockstar's own header explains the format:
//!
//! ```text
//! TASK_NAME PARAMETER_SET_NAME {
//!   PARAMETER_NAME PARAMETER_VALUE      ; comment
//! }
//! ```
//! `*` as a set name is the default set; `*` as a value means "take it from the default set".
//! Values can be several numbers (`balanceAngVelMultiplier 4.0 1.0 4.0`) or `true`/`false`.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct TaskParams {
    /// (task, set) → parameter → raw value text (comments stripped).
    sets: HashMap<(String, String), HashMap<String, String>>,
}

impl TaskParams {
    pub fn parse(text: &str) -> TaskParams {
        let mut sets: HashMap<(String, String), HashMap<String, String>> = HashMap::new();
        let mut current: Option<(String, String)> = None;
        for line in text.lines() {
            let line = line.split(';').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if line.ends_with('{') {
                let mut w = line.trim_end_matches('{').split_whitespace();
                if let (Some(task), Some(set)) = (w.next(), w.next()) {
                    current = Some((task.to_string(), set.to_string()));
                    sets.entry((task.to_string(), set.to_string())).or_default();
                }
            } else if line.starts_with('}') {
                current = None;
            } else if let Some(key) = &current {
                if let Some((name, value)) = line.split_once(char::is_whitespace) {
                    sets.entry(key.clone()).or_default().insert(name.to_string(), value.trim().to_string());
                }
            }
        }
        TaskParams { sets }
    }

    /// The value's text, following `*` to the task's default set.
    pub fn raw(&self, task: &str, set: &str, name: &str) -> Option<&str> {
        let v = self.sets.get(&(task.to_string(), set.to_string()))?.get(name)?;
        if v == "*" {
            if set == "*" {
                return None;
            }
            return self.raw(task, "*", name);
        }
        Some(v.as_str())
    }

    pub fn f32(&self, task: &str, set: &str, name: &str) -> Option<f32> {
        self.raw(task, set, name)?.split_whitespace().next()?.parse().ok()
    }

    pub fn vec3(&self, task: &str, set: &str, name: &str) -> Option<[f32; 3]> {
        let v: Vec<f32> = self.raw(task, set, name)?.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    }

    pub fn tasks(&self) -> usize {
        self.sets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_defaults_and_vectors() {
        let t = TaskParams::parse(
            "; header\nNMBalance * {\n  balanceStiffness 9.0 ; NB. ARMS\n  balanceAngVelMultiplier 4.0 1.0 4.0\n}\n\nNMBalance WEAK {\n  balanceStiffness 8.7\n  balanceAngVelMultiplier *\n}\n",
        );
        assert_eq!(t.f32("NMBalance", "*", "balanceStiffness"), Some(9.0));
        assert_eq!(t.f32("NMBalance", "WEAK", "balanceStiffness"), Some(8.7));
        assert_eq!(t.vec3("NMBalance", "WEAK", "balanceAngVelMultiplier"), Some([4.0, 1.0, 4.0]));
        assert_eq!(t.f32("NMBalance", "*", "missing"), None);
    }
}
