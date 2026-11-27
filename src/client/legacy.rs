//! Clipboard-only import of the browser/Electron JSON save formats.

use std::time::Instant;

use _8times8::{Amount, Coord, Game, LegacyBoard, Level};
use serde_json::{Map, Value};

use super::{machines::Machines, save::Saved};

const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const WORLD_PITCH: f64 = 253.0;
const TILE_PITCH: f64 = 8.0;
const OLD_CAMERA_CENTER: [f64; 2] = [122.5; 2];
const OLD_CELL: f64 = 31.0;
const LAST_MANUAL_STAGE: u64 = 16;
const MAX_LEGACY_MACHINE_STAGE: u64 = i32::MAX as u64;

type Object = Map<String, Value>;
type Bot = (Level, u32, bool, [f64; 2]);
type Camera = ([i64; 2], [f64; 2], f64);

pub(super) fn decode(text: &str, now: Instant) -> Result<Saved, String> {
    if text.len() > MAX_JSON_BYTES {
        return Err("legacy save text exceeds the 16 MiB limit".into());
    }
    let json: Value =
        serde_json::from_str(text).map_err(|err| format!("invalid legacy save JSON: {err}"))?;
    let data = json.as_object().ok_or("legacy save must be an object")?;
    let version = match data.get("v").and_then(Value::as_u64) {
        Some(version @ (1 | 2)) => version,
        _ => return Err("unsupported legacy save version".into()),
    };
    let (camera, world_center) = settings_camera(data)?;
    let dark = match data.get("theme") {
        None | Some(Value::Null) => None,
        Some(theme) if theme.as_str() == Some("dark") => Some(true),
        Some(theme) if theme.as_str() == Some("light") => Some(false),
        _ => return Err("invalid legacy theme".into()),
    };
    let sound = flag(data, "sound", true)?;
    let hint_seen = flag(data, "hintSeen", false)?;
    let (board, bots) = if version == 1 {
        migrate_v1(data, world_center)?
    } else {
        read_v2(data, world_center)?
    };
    let machines = Machines::import_legacy(bots, now)?.encode();
    let board = Game::import_legacy(board)?.encode();
    Ok(Saved {
        board,
        machines,
        dark,
        sound,
        hint_seen,
        camera,
    })
}

fn flag(data: &Object, field: &str, default: bool) -> Result<bool, String> {
    match data.get(field) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("invalid legacy {field} flag")),
    }
}

fn settings_camera(data: &Object) -> Result<(Option<Camera>, [f64; 2]), String> {
    let Some(cam) = data.get("cam") else {
        return Ok((None, OLD_CAMERA_CENTER));
    };
    let cam = cam.as_object().ok_or("invalid legacy camera")?;
    let center = [
        field_number(cam, "x", "camera.x")?,
        field_number(cam, "y", "camera.y")?,
    ];
    let (x, dx) = camera_axis(center[0])?;
    let (y, dy) = camera_axis(center[1])?;
    // The saved native scale is in logical screen pixels, without the DPI factor.
    let scale = field_number(cam, "z", "camera.z")? * (WORLD_PITCH / TILE_PITCH);
    if !scale.is_finite() || scale <= 0.0 {
        return Err("invalid legacy camera zoom".into());
    }
    Ok((Some(([x, y], [dx, dy], scale)), center))
}

fn camera_axis(world: f64) -> Result<(i64, f64), String> {
    let block = world.div_euclid(WORLD_PITCH);
    let min = i64::MIN as f64;
    if !(min..-min).contains(&block) {
        return Err("legacy camera position is out of range".into());
    }
    let offset = world.rem_euclid(WORLD_PITCH) / WORLD_PITCH * TILE_PITCH;
    // A very small negative remainder can round up to a full pitch.
    Ok((block as i64, offset.min(TILE_PITCH.next_down())))
}

fn field_number(data: &Object, field: &str, label: &str) -> Result<f64, String> {
    data.get(field)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("invalid legacy {label}"))
}

fn rows<'a>(value: &'a Value, label: &str) -> Result<&'a [Value], String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("legacy {label} must be an array"))
}

fn row<'a, const N: usize>(value: &'a Value, label: &str) -> Result<&'a [Value; N], String> {
    rows(value, label)?
        .try_into()
        .map_err(|_| format!("legacy {label} must have {N} entries"))
}

fn blocks(data: &Object) -> Result<Vec<(i64, i64, u64)>, String> {
    rows(data.get("blocks").ok_or("missing legacy blocks")?, "blocks")?
        .iter()
        .map(|entry| {
            let [x, y, mask] = row(entry, "block")?;
            Ok((
                x.as_i64().ok_or("invalid legacy block x")?,
                y.as_i64().ok_or("invalid legacy block y")?,
                block_mask(mask.as_str().ok_or("invalid legacy block mask")?)?,
            ))
        })
        .collect()
}

fn block_mask(text: &str) -> Result<u64, String> {
    if text == "f" {
        return Ok(u64::MAX);
    }
    if text.len() != 16 || !text.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("invalid legacy block mask".into());
    }
    // The first word is the low 32 bits (tile indexes 0 through 31).
    let (lo, hi) = text.split_at(8);
    let hex = |s| u64::from_str_radix(s, 16).map_err(|_| "invalid legacy block mask");
    Ok(hex(lo)? | hex(hi)? << 32)
}

fn level(number: u64) -> Option<Level> {
    let index = usize::try_from(number.checked_sub(1)?).ok()?;
    Level::ALL.get(index).copied()
}

fn legacy_coord(level: Level, value: &Value) -> Result<Coord, String> {
    let (x, y) = value
        .as_str()
        .and_then(|key| key.split_once(','))
        .ok_or("invalid legacy coordinate key")?;
    let axis = |s: &str| {
        let digits = s.strip_prefix('-').unwrap_or(s);
        if !digits.bytes().all(|b| b.is_ascii_digit()) || (digits.starts_with('0') && s != "0") {
            return Err("invalid legacy coordinate key");
        }
        s.parse::<i64>()
            .map_err(|_| "invalid legacy coordinate key")
    };
    Ok(Coord::new(level, axis(x)?, axis(y)?))
}

fn by_level<T>(
    data: &Object,
    field: &str,
    allowed: &[Level],
    read: impl Fn(Level, &Value) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    let Some(value) = data.get(field) else {
        return Ok(Vec::new());
    };
    let levels = value
        .as_object()
        .ok_or_else(|| format!("legacy {field} must be an object"))?;
    let mut result = Vec::new();
    for (key, entries) in levels {
        let tier = match key.as_bytes() {
            [digit @ b'1'..=b'8'] => level(u64::from(digit - b'0')),
            _ => None,
        };
        let tier = tier
            .filter(|l| allowed.contains(l))
            .ok_or_else(|| format!("invalid level in legacy {field}"))?;
        for entry in rows(entries, field)? {
            result.push(read(tier, entry)?);
        }
    }
    Ok(result)
}

fn amounts(data: &Object, field: &str) -> Result<[Amount; 8], String> {
    let mut result = std::array::from_fn(|_| Amount::ZERO);
    let Some(value) = data.get(field) else {
        return Ok(result);
    };
    let entries = rows(value, field)?;
    if entries.len() > result.len() + 1 {
        return Err(format!("invalid legacy {field} length"));
    }
    for (i, entry) in entries.iter().enumerate() {
        let amount = entry
            .as_str()
            .ok_or_else(|| format!("legacy {field} amounts must be decimal strings"))?
            .parse::<Amount>()
            .map_err(|err| format!("invalid legacy {field} amount: {err}"))?;
        // Old account arrays are one-based: element zero is never used.
        if i != 0 {
            result[i - 1] = amount;
        } else if amount != Amount::ZERO {
            return Err(format!("invalid level zero amount in legacy {field}"));
        }
    }
    Ok(result)
}

fn bounded_integer(value: &Value, max: u64, label: &str) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|n| *n <= max)
        .ok_or_else(|| format!("invalid legacy {label}"))
}

fn item_state(data: &Object) -> Result<(Option<Coord>, bool), String> {
    let Some(item) = data.get("item") else {
        return Ok((None, false));
    };
    let item = item.as_object().ok_or("invalid legacy item")?;
    let state = bounded_integer(
        item.get("s").ok_or("missing legacy item state")?,
        2,
        "item state",
    )?;
    let x = item
        .get("x")
        .and_then(Value::as_i64)
        .ok_or("invalid legacy item x")?;
    let y = item
        .get("y")
        .and_then(Value::as_i64)
        .ok_or("invalid legacy item y")?;
    Ok((
        (state == 1).then_some(Coord::new(Level::C, x, y)),
        state == 2,
    ))
}

fn bot_position(world: [f64; 2], center: [f64; 2], index: usize) -> [f64; 2] {
    // The old loader uses the purchase stagger when both saved axes are zero.
    let world = if world == [0.0; 2] {
        [
            center[0] + ((index % 3) as f64 - 1.0) * OLD_CELL,
            center[1] + ((index / 3 % 3) as f64 - 1.0) * OLD_CELL,
        ]
    } else {
        world
    };
    world.map(|n| n / WORLD_PITCH * TILE_PITCH)
}

fn read_v2(data: &Object, center: [f64; 2]) -> Result<(LegacyBoard, Vec<Bot>), String> {
    let bots = match data.get("bots") {
        None => Vec::new(),
        Some(value) => {
            let entries = rows(value, "bots")?;
            if entries.len() > 24 {
                return Err("too many legacy machines".into());
            }
            let mut bots = Vec::with_capacity(entries.len());
            let mut migrated = 0;
            for (i, entry) in entries.iter().enumerate() {
                let bot = entry.as_object().ok_or("invalid legacy machine")?;
                let tier = bot
                    .get("t")
                    .and_then(Value::as_u64)
                    .and_then(level)
                    .filter(|&l| l <= Level::E)
                    .ok_or("invalid legacy machine tier")?;
                let stage = bounded_integer(
                    bot.get("s").ok_or("missing legacy machine stage")?,
                    MAX_LEGACY_MACHINE_STAGE,
                    "machine stage",
                )? as u32;
                let legacy = match bot.get("l") {
                    None | Some(Value::Bool(false)) => false,
                    Some(Value::Bool(true)) => true,
                    Some(flag) => bounded_integer(flag, 1, "machine legacy flag")? == 1,
                };
                if legacy {
                    migrated += 1;
                    if tier != Level::A || migrated > 4 {
                        return Err("invalid migrated legacy machines".into());
                    }
                }
                let world = [
                    field_number(bot, "x", "machine.x")?,
                    field_number(bot, "y", "machine.y")?,
                ];
                bots.push((tier, stage, legacy, bot_position(world, center, i)));
            }
            bots
        }
    };
    let initial_stage = data.get("manual").map_or(Ok(0), |value| {
        bounded_integer(value, LAST_MANUAL_STAGE, "manual stage")
    })? as u8;
    let (item, item_held) = item_state(data)?;
    let done = by_level(data, "done", &Level::ALL[1..], legacy_coord)?;
    let large = by_level(data, "coarse", &[Level::B, Level::D, Level::F], |l, e| {
        let [key, progress] = row(e, "coarse entry")?;
        Ok((
            legacy_coord(l, key)?,
            bounded_integer(progress, l.tile_capacity() - 1, "coarse progress")?,
        ))
    })?;
    let groups = by_level(
        data,
        "groups",
        &[Level::C, Level::E, Level::G],
        legacy_coord,
    )?;
    Ok((
        LegacyBoard {
            blocks: blocks(data)?,
            done,
            large,
            groups,
            initial_stage,
            bonus: amounts(data, "bonus")?,
            spent: amounts(data, "spent")?,
            item,
            item_held,
        },
        bots,
    ))
}

fn migrate_v1(data: &Object, center: [f64; 2]) -> Result<(LegacyBoard, Vec<Bot>), String> {
    let upgrades = match data.get("upgrades") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_object().ok_or("invalid legacy upgrades")?),
    };
    let count = |key| match upgrades.and_then(|u| u.get(key)) {
        None | Some(Value::Bool(false)) => Ok(0),
        Some(Value::Bool(true)) => Ok(1),
        Some(n) => bounded_integer(n, i32::MAX as u64, key),
    };
    let [double, quad, octo, all64, auto, auto_b, bot_double] = [
        count("double")?,
        count("quad")?,
        count("octo")?,
        count("all64")?,
        count("auto")?,
        count("autoB")?,
        count("botDouble")?,
    ];
    let initial_stage = [double, quad, octo, all64]
        .iter()
        .rposition(|&n| n != 0)
        .map_or(0, |i| i as u8 + 1);
    // Preserve the actual old purchases rather than charging modern prices or
    // deriving the total from the highest acquired manual stage.
    let mut spent = std::array::from_fn(|_| Amount::ZERO);
    spent[0] = Amount::from(u128::from(
        8 * double + 8 * quad + 48 * octo + 80 * auto.min(4) + 160 * bot_double,
    ));
    spent[1] = Amount::from(u128::from(8 * all64 + 8 * auto_b.min(4)));

    let positions = match data.get("bots") {
        Some(value) => rows(value, "bots")?,
        None => match data.get("bot") {
            None | Some(Value::Null) => &[],
            Some(value) => std::slice::from_ref(value),
        },
    };
    if positions.len() > 8 {
        return Err("too many version 1 machine positions".into());
    }
    let position = |entry: &Value| -> Result<[f64; 2], String> {
        let bot = entry.as_object().ok_or("invalid legacy machine")?;
        let axis = |key, label| {
            if bot.contains_key(key) {
                field_number(bot, key, label)
            } else {
                Ok(0.0)
            }
        };
        Ok([axis("x", "machine.x")?, axis("y", "machine.y")?])
    };
    let positions = positions
        .iter()
        .map(position)
        .collect::<Result<Vec<_>, _>>()?;
    let purchased = auto.min(4) + if auto >= 4 { auto_b.min(4) } else { 0 };
    let bots = (0..purchased)
        .map(|i| {
            let world = positions.get(i as usize).copied().unwrap_or([0.0; 2]);
            (
                Level::A,
                u32::from(i < bot_double),
                i >= 4,
                bot_position(world, center, i as usize),
            )
        })
        .collect();
    Ok((
        LegacyBoard {
            blocks: blocks(data)?,
            done: Vec::new(),
            large: Vec::new(),
            groups: Vec::new(),
            initial_stage,
            bonus: std::array::from_fn(|_| Amount::ZERO),
            spent,
            item: None,
            item_held: false,
        },
        bots,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_one_migrates_purchases_and_keeps_machine_order() {
        let now = Instant::now();
        let saved = decode(
            r#"{
                "v":1, "blocks":[[0,0,"0000000100000002"]],
                "upgrades":{"double":true,"octo":1,"all64":true,"auto":4,"autoB":4,"botDouble":3},
                "bots":[{"x":-253,"y":506}], "cam":{"x":253,"y":-253,"z":2},
                "sound":false, "hintSeen":true, "theme":"dark"
            }"#,
            now,
        )
        .unwrap();
        let mut game = Game::decode(&saved.board).unwrap();
        assert_eq!(game.block_at(0, 0).unwrap().mask(), 1 | (1 << 33));
        assert_eq!(game.manual_stage(), 4);
        assert!(game.add_debug_bonus(Level::A, &Amount::from(1000)));
        assert!(game.add_debug_bonus(Level::B, &Amount::from(50)));
        assert_eq!(game.available(Level::A).to_string(), "144");
        assert_eq!(game.available(Level::B).to_string(), "10");
        assert_eq!(saved.dark, Some(true));
        assert!(!saved.sound);
        assert!(saved.hint_seen);
        assert_eq!(saved.camera, Some(([1, -1], [0.0, 0.0], 63.25)));

        let machines = Machines::decode(&saved.machines, now).unwrap();
        assert!(
            machines
                .entries()
                .eq((0..8).map(|i| (i, Level::A, u32::from(i < 3), i >= 4)))
        );
        let mut positions = machines.views(now).map(|(_, p)| p);
        let first = positions.next().unwrap();
        let second = positions.next().unwrap();
        for (p, expected) in [(first, [-8.0, 16.0]), (second, [8.0, -284.0 / 253.0 * 8.0])] {
            assert!(
                p.into_iter()
                    .zip(expected)
                    .all(|(p, expected)| (p - expected).abs() < 1e-12)
            );
        }
    }

    #[test]
    fn version_one_accepts_the_old_single_machine_and_only_unlocks_extra_rows_after_four() {
        let now = Instant::now();
        let saved = decode(
            r#"{"v":1,"blocks":[[0,0,"f"]],"upgrades":{"auto":true,"autoB":4},"bot":{"x":253,"y":-253}}"#,
            now,
        )
        .unwrap();
        let game = Game::decode(&saved.board).unwrap();
        assert_eq!(game.cleared(Level::A), &Amount::from(1));
        let machines = Machines::decode(&saved.machines, now).unwrap();
        assert!(machines.entries().eq([(0, Level::A, 0, false)]));
        assert_eq!(machines.views(now).next().unwrap().1, [8.0, -8.0]);
        assert_eq!(saved.dark, None);
        assert_eq!(saved.camera, None);
        assert!(saved.sound);
        assert!(!saved.hint_seen);
    }

    #[test]
    fn version_two_restores_coarse_progress_item_and_exact_amounts() {
        let now = Instant::now();
        let saved = decode(
            r#"{
                "v":2, "sound":false, "hintSeen":true, "theme":null,
                "cam":{"x":-0.25,"y":506,"z":1.25}, "manual":16,
                "spent":["0","9007199254740992","0","0","0","0","0","0","340282366920938463463374607431768211456"],
                "bonus":["0","9007199254740993","0","0","0","0","0","0","340282366920938463463374607431768211458"],
                "blocks":[], "done":{"4":["0,0"]},
                "groups":{"3":["0,0"]}, "coarse":{"2":[["-45,-36",123]]},
                "bots":[{"t":1,"s":1,"l":1,"x":-253,"y":506},{"t":5,"s":12,"l":0,"x":15,"y":10}],
                "item":{"s":1,"x":5,"y":0}
            }"#,
            now,
        )
        .unwrap();
        let game = Game::decode(&saved.board).unwrap();
        let large = Coord::new(Level::B, -45, -36);
        let at = large.bounds_in(Level::A).unwrap().min();
        assert_eq!(game.large_at(at.x(), at.y()), Some((large, 123)));
        assert!(game.large_group(large.parent().unwrap()));
        assert_eq!(game.item(), Some(Coord::new(Level::C, 5, 0)));
        assert_eq!(game.manual_stage(), 16);
        assert_eq!(game.cleared(Level::A), &Amount::from(262_144));
        assert_eq!(game.cleared(Level::B), &Amount::from(4096));
        assert_eq!(game.cleared(Level::C), &Amount::from(64));
        assert_eq!(game.cleared(Level::D), &Amount::from(1));
        assert_eq!(game.available(Level::A).to_string(), "262145");
        assert_eq!(game.available(Level::H).to_string(), "2");
        let machines = Machines::decode(&saved.machines, now).unwrap();
        assert!(
            machines
                .entries()
                .eq([(0, Level::A, 1, true), (1, Level::E, 12, false)])
        );
        assert!(saved.dark.is_none());
        assert!(!saved.sound);
        assert!(saved.hint_seen);
        let (blocks, offsets, scale) = saved.camera.unwrap();
        assert_eq!(blocks, [-1, 2]);
        assert_eq!(offsets, [252.75 / 253.0 * 8.0, 0.0]);
        assert_eq!(scale, 1.25 * 253.0 / 8.0);
    }

    #[test]
    fn version_two_restores_held_item_and_explicit_light_theme() {
        let saved = decode(
            r#"{"v":2,"blocks":[],"theme":"light","item":{"s":2,"x":0,"y":0}}"#,
            Instant::now(),
        )
        .unwrap();
        assert!(Game::decode(&saved.board).unwrap().has_drag_item());
        assert_eq!(saved.dark, Some(false));
    }

    #[test]
    fn rejects_unsupported_malformed_and_oversized_data() {
        let now = Instant::now();
        for text in [
            r#"{"v":3,"blocks":[]}"#,
            r#"{"v":2,"blocks":["#,
            r#"{"v":2,"blocks":[]}null"#,
            r#"{"v":2,"item":{"s":2,"x":0,"y":0}}"#,
            r#"{"v":2,"blocks":[[0,0]]}"#,
            r#"{"v":2,"blocks":[[0,0,"0000000G00000000"]]}"#,
            r#"{"v":2,"blocks":[[0,0,"f"],[0,0,"f"]]}"#,
            r#"{"v":2,"blocks":[],"spent":["0","-1"]}"#,
            r#"{"v":2,"blocks":[],"bonus":["0",9007199254740993]}"#,
            r#"{"v":2,"blocks":[],"manual":-1}"#,
            r#"{"v":2,"blocks":[],"groups":{"8":["0,0"]}}"#,
            r#"{"v":2,"blocks":[],"done":{"2":["0,0,0"]}}"#,
            r#"{"v":2,"blocks":[],"coarse":{"2":[["0,0",4096]]}}"#,
            r#"{"v":2,"blocks":[],"coarse":{"4":[["0,0",-1]]}}"#,
            r#"{"v":2,"blocks":[],"bots":[{"t":6,"s":1,"x":0,"y":0}]}"#,
            r#"{"v":2,"blocks":[],"bots":[{"t":1,"s":-1,"x":0,"y":0}]}"#,
            r#"{"v":2,"blocks":[],"cam":{"x":0,"y":0,"z":0}}"#,
            r#"{"v":2,"blocks":[],"cam":{"x":1e100,"y":0,"z":1}}"#,
            r#"{"v":1,"blocks":[],"upgrades":{"auto":-1}}"#,
        ] {
            assert!(decode(text, now).is_err(), "accepted {text}");
        }
        let oversized = " ".repeat(MAX_JSON_BYTES + 1);
        assert!(decode(&oversized, now).unwrap_err().contains("16 MiB"));
    }
}
