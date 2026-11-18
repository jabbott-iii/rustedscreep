//! The base planner's terrain corpus (ROADMAP P4.1): rooms exported from the official MMO, one file per
//! room under `tests/fixtures/rooms/<shard>/`, written by `js/tools/rooms-fixture.mjs` (the format and
//! where the data came from are in that directory's README.md).
//!
//! A malformed file panics with its path and line: the corpus is checked-in test data, so a bad file is
//! a test failure, not something to skip.

use std::fs;
use std::path::{Path, PathBuf};

use rustedscreep_consts::{ROOM_AREA, ROOM_SIZE, RoomName};
use rustedscreep_core::movement::{TerrainGrid, Tile};

/// One room of the corpus: its terrain and the natural objects the planner places around.
pub struct CorpusRoom {
    pub file: PathBuf,
    pub shard: String,
    pub name: RoomName,
    /// `ROOM_AREA` raw terrain masks in row-major order (`y * 50 + x`), byte for byte what
    /// `Room.Terrain.getRawBuffer()` returns for the room (V156, V224, V226).
    pub raw: Vec<u8>,
    pub controller: Tile,
    pub sources: Vec<Tile>,
    /// Each mineral's type (the server's one-letter resource name) and tile.
    pub minerals: Vec<(String, Tile)>,
}

impl CorpusRoom {
    /// The room's terrain as the movement and planner code read it.
    pub fn terrain(&self) -> TerrainGrid {
        TerrainGrid::from_row_major(&self.raw)
            .unwrap_or_else(|| panic!("{}: not ROOM_AREA bytes", self.file.display()))
    }

    /// The raw mask at `t`. Uses: ROOM_SIZE (the row-major index, V156).
    pub fn mask(&self, t: Tile) -> u8 {
        self.raw[usize::from(t.y()) * usize::from(ROOM_SIZE) + usize::from(t.x())]
    }

    /// The controller, the sources and the minerals, in that order.
    pub fn objects(&self) -> impl Iterator<Item = Tile> + '_ {
        std::iter::once(self.controller)
            .chain(self.sources.iter().copied())
            .chain(self.minerals.iter().map(|&(_, t)| t))
    }
}

/// `tests/fixtures/rooms`.
pub fn rooms_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("rooms")
}

/// Every room of the corpus, sorted by shard and then by file name (so every caller sees one order).
pub fn load() -> Vec<CorpusRoom> {
    let mut files = Vec::new();
    for shard in read_dir(&rooms_dir()) {
        if shard.is_dir() {
            files.extend(
                read_dir(&shard)
                    .into_iter()
                    .filter(|f| f.extension().is_some_and(|e| e == "txt")),
            );
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text =
                fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            parse(file, &text)
        })
        .collect()
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
                .path()
        })
        .collect()
}

/// Uses: ROOM_SIZE (50 rows of 50 digits), ROOM_AREA.
fn parse(file: PathBuf, text: &str) -> CorpusRoom {
    let at = |n: usize, why: &str| -> ! { panic!("{}:{}: {why}", file.display(), n + 1) };
    let tile = |n: usize, x: Option<&str>, y: Option<&str>| -> Tile {
        let coordinate = |v: Option<&str>| v.and_then(|v| v.parse::<u8>().ok());
        coordinate(x)
            .zip(coordinate(y))
            .and_then(|(x, y)| Tile::new(x, y))
            .unwrap_or_else(|| at(n, "not a tile"))
    };
    let (mut room, mut shard, mut controller) = (None, None, None);
    let (mut sources, mut minerals, mut raw) = (Vec::new(), Vec::new(), Vec::new());
    let mut rows = None::<usize>;
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(count) = rows.as_mut() {
            if line.len() != usize::from(ROOM_SIZE) {
                at(n, "a terrain row is not ROOM_SIZE digits");
            }
            for c in line.chars() {
                raw.push(
                    c.to_digit(10)
                        .and_then(|d| u8::try_from(d).ok())
                        .unwrap_or_else(|| at(n, "not a digit")),
                );
            }
            *count += 1;
            continue;
        }
        let mut words = line.split(' ');
        match words.next() {
            Some("room") => {
                room = Some(
                    words
                        .next()
                        .and_then(|r| r.parse::<RoomName>().ok())
                        .unwrap_or_else(|| at(n, "bad room")),
                );
            }
            Some("shard") => shard = words.next().map(str::to_owned),
            Some("controller") => controller = Some(tile(n, words.next(), words.next())),
            Some("source") => sources.push(tile(n, words.next(), words.next())),
            Some("mineral") => {
                let kind = words
                    .next()
                    .unwrap_or_else(|| at(n, "no mineral type"))
                    .to_owned();
                minerals.push((kind, tile(n, words.next(), words.next())));
            }
            Some("terrain") => rows = Some(0),
            _ => at(n, "unknown line"),
        }
        if words.next().is_some() {
            at(n, "extra words");
        }
    }
    let name = room.unwrap_or_else(|| panic!("{}: no room line", file.display()));
    let shard = shard.unwrap_or_else(|| panic!("{}: no shard line", file.display()));
    let controller = controller.unwrap_or_else(|| panic!("{}: no controller line", file.display()));
    assert_eq!(
        rows,
        Some(usize::from(ROOM_SIZE)),
        "{}: terrain rows",
        file.display()
    );
    assert_eq!(raw.len(), ROOM_AREA, "{}", file.display());
    let stem = file.file_stem().and_then(|s| s.to_str());
    assert_eq!(
        stem,
        Some(name.to_string().as_str()),
        "{}: the file is named after its room",
        file.display()
    );
    let dir = file
        .parent()
        .and_then(Path::file_name)
        .and_then(|d| d.to_str());
    assert_eq!(
        dir,
        Some(shard.as_str()),
        "{}: the directory is named after its shard",
        file.display()
    );
    CorpusRoom {
        file,
        shard,
        name,
        raw,
        controller,
        sources,
        minerals,
    }
}
