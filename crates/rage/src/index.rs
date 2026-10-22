//! Where everything lives: an index over every map archive and definition file in the base game,
//! so a chunk can pull models and texture packs from wherever they are stored.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::crypto::Key;
use crate::drawable::{self, Drawable};
use crate::img::Img;
use crate::placement::{self, hash};
use crate::rsc::Resource;
use crate::wtd::{self, Texture};

pub struct GameIndex {
    pub imgs: Vec<Img>,
    /// model name hash → (archive, entry) for .wdr files.
    pub models: HashMap<u32, (usize, usize)>,
    /// .wdd drawable dictionaries: (archive, entry). Searched when a hash has no .wdr.
    pub dictionaries: Vec<(usize, usize)>,
    /// texture pack name (lowercase, no extension) → (archive, entry).
    pub txds: HashMap<String, (usize, usize)>,
    /// texture pack → its parent pack (IDE `txdp` sections).
    pub txd_parent: HashMap<String, String>,
    /// model name hash → texture pack name (IDE `objs`/`tobj`/`anim`).
    pub model_txd: HashMap<u32, String>,
    /// model name hash → name, for messages.
    pub model_name: HashMap<u32, String>,
    /// model name hash → IDE draw distance (metres).
    pub model_draw_dist: HashMap<u32, f32>,
}

impl GameIndex {
    /// Index `pc/data/maps/**` (archives + IDEs) and `common/data/*.ide` of the base game.
    pub fn build(root: &Path, key: &Key) -> Result<GameIndex> {
        let mut img_paths = Vec::new();
        let mut ide_paths = Vec::new();
        collect(&root.join("pc/data/maps"), &mut img_paths, &mut ide_paths)?;
        // Shared parent texture packs (mh_roads, mh_buildings, ...) live here, not with the maps.
        img_paths.push(root.join("pc/data/cdimages/gtxd.img"));
        for f in std::fs::read_dir(root.join("common/data"))? {
            let p = f?.path();
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ide")) {
                ide_paths.push(p);
            }
        }

        let mut idx = GameIndex {
            imgs: Vec::new(),
            models: HashMap::new(),
            dictionaries: Vec::new(),
            txds: HashMap::new(),
            txd_parent: HashMap::new(),
            model_txd: HashMap::new(),
            model_name: HashMap::new(),
            model_draw_dist: HashMap::new(),
        };
        for p in &img_paths {
            let img = Img::open(p, key)?;
            let a = idx.imgs.len();
            for (e, entry) in img.entries.iter().enumerate() {
                let stem = entry.name.rsplit_once('.').map_or(&*entry.name, |(s, _)| s).to_ascii_lowercase();
                match entry.ext() {
                    "wdr" | "wft" => {
                        idx.models.insert(hash(&stem), (a, e));
                    }
                    "wdd" => idx.dictionaries.push((a, e)),
                    "wtd" => {
                        idx.txds.insert(stem, (a, e));
                    }
                    _ => {}
                }
            }
            idx.imgs.push(img);
        }
        for p in &ide_paths {
            let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            for def in placement::read_ide(&text) {
                let h = hash(&def.name);
                idx.model_txd.insert(h, def.txd);
                idx.model_draw_dist.insert(h, def.draw_distance);
                idx.model_name.insert(h, def.name);
            }
            for (child, parent) in read_txdp(&text) {
                idx.txd_parent.insert(child, parent);
            }
        }
        Ok(idx)
    }

    pub fn resource(&self, (a, e): (usize, usize)) -> Result<Resource> {
        let img = &self.imgs[a];
        Resource::load(&img.read(&img.entries[e])?)
            .with_context(|| format!("{} in {}", img.entries[e].name, img.path.display()))
    }

    pub fn load_txd(&self, name: &str) -> Result<Option<Vec<Texture>>> {
        match self.txds.get(name) {
            Some(&loc) => Ok(Some(wtd::read(&self.resource(loc)?)?)),
            None => Ok(None),
        }
    }

    /// A texture pack followed by its parents, nearest first.
    pub fn txd_chain(&self, name: &str) -> Vec<String> {
        let mut chain = vec![name.to_string()];
        while let Some(p) = self.txd_parent.get(chain.last().unwrap()) {
            if chain.contains(p) || chain.len() > 8 {
                break;
            }
            chain.push(p.clone());
        }
        chain
    }

    /// Load drawables for the given model hashes: .wdr files directly, the rest from .wdd
    /// dictionaries (restricted to `dict_archives` to keep it quick). Returns found drawables.
    pub fn load_models(&self, hashes: &[u32], dict_archives: &[usize]) -> Result<HashMap<u32, Drawable>> {
        let mut out = HashMap::new();
        for &h in hashes {
            if let Some(&(a, e)) = self.models.get(&h) {
                let res = self.resource((a, e))?;
                let d = if self.imgs[a].entries[e].ext() == "wft" {
                    drawable::read_fragment(&res)
                } else {
                    drawable::read(&res)
                };
                out.insert(h, d.with_context(|| format!("model {}", self.imgs[a].entries[e].name))?);
            }
        }
        if out.len() < hashes.len() {
            for &(a, e) in self.dictionaries.iter().filter(|(a, _)| dict_archives.contains(a)) {
                for (h, d) in drawable::read_dictionary(&self.resource((a, e))?)? {
                    if hashes.contains(&h) {
                        out.entry(h).or_insert(d);
                    }
                }
            }
        }
        Ok(out)
    }

    pub fn archive(&self, rel: &str) -> Option<usize> {
        let want = rel.replace('\\', "/").to_ascii_lowercase();
        self.imgs.iter().position(|i| {
            i.path.to_string_lossy().replace('\\', "/").to_ascii_lowercase().ends_with(&want)
        })
    }
}

fn collect(dir: &Path, imgs: &mut Vec<std::path::PathBuf>, ides: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            collect(&p, imgs, ides)?;
        } else {
            match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
                Some("img") => imgs.push(p),
                Some("ide") => ides.push(p),
                _ => {}
            }
        }
    }
    Ok(())
}

/// `txdp` section: `child, parent` per line.
fn read_txdp(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        match line {
            "txdp" => inside = true,
            "end" => inside = false,
            _ if inside => {
                if let Some((c, p)) = line.split_once(',') {
                    out.push((c.trim().to_ascii_lowercase(), p.trim().to_ascii_lowercase()));
                }
            }
            _ => {}
        }
    }
    out
}
