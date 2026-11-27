/* 8 times 8 — a quiet tile-flipping board. */
(() => {
  'use strict';

  // ---------- Geometry (world units) ----------
  const N = 8;                         // tiles per side
  const TILE = 28;
  const GAP = 3;
  const CELL = TILE + GAP;
  const BLOCK = N * TILE + (N - 1) * GAP; // 245
  const BLOCK_GAP = 8;
  const PITCH = BLOCK + BLOCK_GAP;
  const RADIUS = 3.5;
  const FULL = 0xFFFFFFFF;

  // The furthest zoom-out is not fixed: it always just fits every open 1A plus the openable ring around them.
  const MAX_ZOOM = 4;

  const FLIP_MS = 190;
  const FADE_IN_MS = 700;
  const DONE_FX_MS = 1800;
  const NEW_HL_MS = 4200;   // gentle pulse on blocks opened by a clear
  const NEW_HL_PERIOD = 1400;

  const PALETTES = {
    light: { bg: '#e4e4e2', front: '#fbfbf9', back: '#2e2e30', halo: '46,46,48', ghost: 'rgba(46,46,48,0.16)', section: 'rgba(46,46,48,0.14)', groupLine: 'rgba(46,46,48,0.5)', botEdge: 'rgba(78,83,92,0.55)' },
    // Dark: dark ground, slightly dimmed white tiles; flipped faces stay black.
    dark:  { bg: '#2a2a2c', front: '#e2e2de', back: '#131315', halo: '200,200,196', ghost: 'rgba(226,226,222,0.12)', section: 'rgba(226,226,222,0.11)', groupLine: 'rgba(150,150,146,0.75)', botEdge: 'rgba(230,232,236,0.5)' },
  };
  const hexRgb = (h) => [1, 3, 5].map((k) => parseInt(h.slice(k, k + 2), 16));
  for (const p of Object.values(PALETTES)) { p.frontRgb = hexRgb(p.front); p.backRgb = hexRgb(p.back); }
  let pal = PALETTES.light;

  const STORAGE_KEY = '8times8.v1';           // same key as before; the payload carries its own version
  const BACKUP_KEY = '8times8.v1.backup';     // untouched copy of a pre-migration save

  const key = (x, y) => x + ',' + y;
  const NEIGHBOURS = [[1, 0], [-1, 0], [0, 1], [0, -1]];
  const clamp = (v, a, b) => v < a ? a : v > b ? b : v;

  // ---------- Levels: 1A, 1B, 1C, 1D, 1E, ... ----------
  // Level 1 = 1A: a block of 8x8 tiles. Level 2 = 1B: 8x8 of 1A. Level 3 = 1C: 8x8 of 1B. And so on.
  // Index of the level-(L+1) unit = floor((index at L + 4) / 8), so the start block sits near the centre at every level.
  const G = 8, G_OFF = 4, G_UNITS = G * G;
  const MAX_LEVEL = 8;   // 1H: 8^7 blocks across. 64^8 tiles still fits exactly in a double.
  const TIERS = 5;       // machines exist for A … E
  const upLevel = (u) => Math.floor((u + G_OFF) / G);
  const letter = (L) => String.fromCharCode(64 + L);  // 1 → A, 2 → B, ...
  const levelSpan = (L) => Math.pow(G, L - 1);          // blocks per side of a level-L unit
  const TILES = [];                                     // TILES[L]: tiles in one level-L unit (64^L)
  for (let L = 0; L <= MAX_LEVEL; L++) TILES[L] = Math.pow(64, L);
  /** First block index (x or y) of level-L unit u. */
  function unitFirstBlock(u, L) { let x = u; for (let l = L; l > 1; l--) x = x * G - G_OFF; return x; }

  // ---------- State ----------
  // blocks: open 1A not yet swallowed by a finished 1B (or higher). Finished higher units are kept
  // only as one entry each in doneSet, so a huge finished area costs almost nothing.
  /** @type {Map<string, {x:number,y:number,lo:number,hi:number,count:number,done:boolean,born:number,anim:Map<number,number>|null,hl?:number}>} */
  const blocks = new Map();
  const doneSet = Array.from({ length: MAX_LEVEL + 1 }, () => new Set());     // [L≥2]: finished units (outermost only)
  const black = Array.from({ length: MAX_LEVEL + 1 }, () => new Map());       // [L≥2]: unit → black tiles inside
  const childDone = Array.from({ length: MAX_LEVEL + 1 }, () => new Map());   // [L≥2]: unit → finished children
  const descDone = Array.from({ length: MAX_LEVEL + 1 }, () => new Map());    // [L≥2]: unit → finished descendants per level
  const levelUnits = Array.from({ length: MAX_LEVEL + 1 }, () => new Set());  // [L≥2]: units holding an open block
  const cleared = new Array(MAX_LEVEL + 1).fill(0);   // finished units per level (exact integers)
  const spent = new Array(MAX_LEVEL + 1).fill(0n);    // points paid per level (BigInt: prices grow ×3 forever)
  let manualLevel = 0;                                // manual upgrade stage, 0 … MANUAL_MAX
  const bots = [];                                    // machines (see "Machines")
  let loading = false;
  // The drag item: appears once, at the centre of a 1C next to the second 1C cleared; finishing that 1C picks it up.
  // s: 0 = not yet, 1 = waiting on the board (x, y = that 1C), 2 = picked up → long-press drag flips tiles.
  const ITEM_LEVEL = 3;
  const item = { s: 0, x: 0, y: 0, t0: 0 };

  let resetting = false;
  let soundOn = true;
  let hintSeen = false;
  let themePref = null; // null = follow system, 'light' | 'dark' = chosen by player

  const cam = { x: BLOCK / 2, y: BLOCK / 2, z: 1 };
  let tween = null;
  let camTouched = false; // false until the camera was restored or moved by the player
  const effects = []; // completion halos
  let animCount = 0;  // tiles currently flipping

  function makeBlock(x, y, lo = 0, hi = 0, born = 0) {
    lo >>>= 0; hi >>>= 0;
    return { x, y, lo, hi, count: popcount(lo) + popcount(hi), done: false, born, anim: null };
  }

  function popcount(v) {
    v = v - ((v >>> 1) & 0x55555555);
    v = (v & 0x33333333) + ((v >>> 2) & 0x33333333);
    return (((v + (v >>> 4)) & 0x0F0F0F0F) * 0x01010101) >>> 24;
  }

  const isBlack = (b, i) => i < 32 ? ((b.lo >>> i) & 1) === 1 : ((b.hi >>> (i - 32)) & 1) === 1;

  // ---------- Points ----------
  const debugBonus = new Array(MAX_LEVEL + 1).fill(0n); // debug panel only: extra points, kept apart from what was earned
  const avail = (L) => { const v = BigInt(cleared[L]) + debugBonus[L] - spent[L]; return v > 0n ? v : 0n; };
  const canPay = (L, cost) => avail(L) >= cost;
  function pay(L, cost) { if (!canPay(L, cost)) return false; spent[L] += cost; return true; }

  /** Readable big numbers: commas up to 999,999,999, then 億 / 兆 / 京 / 垓, then e-notation. */
  function fmt(v) {
    const b = typeof v === 'bigint' ? v : BigInt(Math.floor(v));
    if (b < 1000000000n) return b.toLocaleString('en-US');
    const units = [[10n ** 20n, '垓'], [10n ** 16n, '京'], [10n ** 12n, '兆'], [10n ** 8n, '億']];
    for (const [u, name] of units) {
      if (b >= u && b < u * 10000n) {
        const hundredths = (b * 100n) / u;
        const whole = hundredths / 100n, frac = hundredths % 100n;
        return whole.toLocaleString('en-US') + (frac ? '.' + frac.toString().padStart(2, '0').replace(/0$/, '') : '') + name;
      }
    }
    const s = b.toString();
    return s[0] + '.' + s.slice(1, 3) + 'e' + (s.length - 1);
  }

  /** Work amount is always 2^e tiles. Label it in the largest whole unit: 8枚, 1A, 32A, 1B … */
  function capLabel(e) {
    if (e < 6) return Math.pow(2, e) + '枚';
    const k = Math.floor(e / 6);
    return Math.pow(2, e - 6 * k) + letter(k);
  }
  const capOf = (e) => e > 52 ? Math.pow(2, 52) : Math.pow(2, e); // ≥ 2^52 tiles is "everything" anyway

  // ---------- Manual upgrades ----------
  // Stage n turns 2^manualExp(n) tiles per click: 1, 2, 4, 8, 1A, 2A, 4A, 8A, 1B, … , 8C, 1D.
  const MANUAL_MAX = 16;
  function manualExp(n) {
    if (n < 4) return n;
    const k = n - 4;
    return 6 * (1 + Math.floor(k / 4)) + (k % 4);
  }
  /** Price of going from stage n to n+1: 8A, 8A, 48A, then per unit B, C, D: 8, 8, 8, 48; finally 8E. */
  function manualPrice(n) {
    if (n < 3) return { unit: 1, cost: [8n, 8n, 48n][n] };
    const g = Math.floor((n - 3) / 4);
    return { unit: 2 + g, cost: (n - 3) % 4 === 3 ? 48n : 8n };
  }

  // ---------- Machines ----------
  // tier 1…5 = A…E machine. One action turns up to 64^(tier-1) · 2^stage white tiles.
  // Body: 80 of its own points. Stage n+1 costs 20·3^n of its own points, forever.
  const MACHINE_PRICE = 80n;
  const MACHINES_PER_TIER = 4;
  const upgradeCost = (stage) => 20n * 3n ** BigInt(stage);
  const botExp = (bot) => 6 * (bot.tier - 1) + bot.stage;
  let nextBotId = 1;

  // ---------- Unit bookkeeping ----------
  /** Is (L, ux, uy) inside a finished unit of a higher level? */
  function coveredByDone(L, ux, uy) {
    for (let m = L + 1; m <= MAX_LEVEL; m++) {
      ux = upLevel(ux); uy = upLevel(uy);
      if (doneSet[m].has(key(ux, uy))) return true;
    }
    return false;
  }
  function unitComplete(L, ux, uy) {
    if (L === 1) { const b = blocks.get(key(ux, uy)); if (b && b.done) return true; }
    else if (doneSet[L].has(key(ux, uy))) return true;
    return coveredByDone(L, ux, uy);
  }
  // Big tiles: once 1D is reached, newly opened ground comes as whole 1B tiles (from 1F: whole 1D tiles, and so on).
  // coarse[L]: level-L units open as one big tile → { born, hl }. Their progress is black[L] (tiles turned so far).
  const coarse = Array.from({ length: MAX_LEVEL + 1 }, () => new Map());
  let coarseCount = 0;
  // bigGroup[P]: level-P units whose children opened as big tiles (the big-tile version of a 1A).
  const bigGroup = Array.from({ length: MAX_LEVEL + 2 }, () => new Set());
  const bigAnim = new Map(); // 'L:x,y' → flip start time of a big tile turning black
  const bigAnimShown = new Set(); // per frame: flipping big tiles left out of the finished-area pass
  function unitAt(L, x, y) { for (let l = 2; l <= L; l++) { x = upLevel(x); y = upLevel(y); } return [x, y]; }
  /** The big tile containing block (x, y), as [L, ux, uy], or null. */
  function coarseAt(x, y) {
    if (!coarseCount) return null;
    for (let L = 2; L <= MAX_LEVEL; L++) {
      x = upLevel(x); y = upLevel(y);
      if (coarse[L].has(key(x, y))) return [L, x, y];
    }
    return null;
  }
  const isOpenArea = (x, y) => blocks.has(key(x, y)) || coveredByDone(1, x, y) || coarseAt(x, y) !== null;
  /** Big-tile size for newly opened ground: 1 (normal 1A) until 1D is reached, then 1B; from 1F 1D; from 1H 1F. */
  function coarseLevel() {
    let K = 0;
    for (let L = MAX_LEVEL; L >= 1; L--) if (cleared[L] > 0) { K = L; break; }
    return K >= 4 ? 2 * Math.floor((K - 2) / 2) : 1;
  }

  // Openness of 1B and higher only changes when such a unit finishes, so it is cached until then.
  const playCache = new Map();
  /** A level-L unit is enterable if it's the start unit or touches (up/down/left/right) a finished level-L unit. */
  function unitOpen(L, ux, uy) {
    if (ux === 0 && uy === 0) return true;
    for (const [dx, dy] of NEIGHBOURS) if (unitComplete(L, ux + dx, uy + dy)) return true;
    return false;
  }
  /** A block can be played only if every enclosing unit (1B, 1C, ...) is enterable. */
  function blockPlayable(x, y) {
    let ux = x, uy = y;
    for (let L = 2; L <= MAX_LEVEL; L++) {
      ux = upLevel(ux); uy = upLevel(uy);
      const ck = L + ':' + ux + ',' + uy;
      let v = playCache.get(ck);
      if (v === undefined) { v = unitOpen(L, ux, uy); playCache.set(ck, v); }
      if (!v) return false;
      if (ux === 0 && uy === 0) return true; // every higher unit is the start unit too
    }
    return true;
  }

  /** Is (L, ux, uy) untouched — nothing open or finished in it yet? */
  const freshUnit = (L, ux, uy) => !levelUnits[L].has(key(ux, uy)) && !unitComplete(L, ux, uy);
  /** Where block (x, y) would open now: a big tile {L, ux, uy} (largest fresh size allowed), or null for a normal 1A. */
  function coarseTargetFor(x, y) {
    const c = coarseLevel();
    for (let L = c; L >= 2; L -= 2) {
      const [ux, uy] = unitAt(L, x, y);
      if (freshUnit(L, ux, uy)) return { L, ux, uy };
    }
    return null;
  }
  /** Are all units above level L around (ux, uy) enterable? (Used for big-tile ghosts.) */
  function unitPlayableAbove(L, ux, uy) {
    for (let m = L + 1; m <= MAX_LEVEL; m++) {
      ux = upLevel(ux); uy = upLevel(uy);
      if (!unitOpen(m, ux, uy)) return false;
      if (ux === 0 && uy === 0) return true;
    }
    return true;
  }
  function openCoarse(L, ux, uy, born) {
    const k = key(ux, uy);
    if (coarse[L].has(k) || unitComplete(L, ux, uy)) return false;
    coarse[L].set(k, { born, hl: 0 });
    coarseCount++;
    notePresence(L, ux, uy);
    const x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L), span = levelSpan(L);
    growBox(x0, y0, x0 + span - 1, y0 + span - 1);
    return true;
  }

  /** Add n black tiles to every unit above (L, ux, uy). */
  function markBlack(L, ux, uy, n) {
    for (let m = L + 1; m <= MAX_LEVEL; m++) {
      ux = upLevel(ux); uy = upLevel(uy);
      const k = key(ux, uy);
      black[m].set(k, (black[m].get(k) || 0) + n);
    }
  }

  // Bounding box (in block indices) of all open blocks.
  const bbox = { x0: 0, x1: 0, y0: 0, y1: 0 };
  function growBox(x0, y0, x1, y1) {
    if (x0 < bbox.x0) bbox.x0 = x0; if (x1 > bbox.x1) bbox.x1 = x1;
    if (y0 < bbox.y0) bbox.y0 = y0; if (y1 > bbox.y1) bbox.y1 = y1;
  }
  function notePresence(L, ux, uy) {
    for (let m = L; m <= MAX_LEVEL; m++) {
      if (m > L) { ux = upLevel(ux); uy = upLevel(uy); }
      if (m >= 2) levelUnits[m].add(key(ux, uy));
    }
  }

  function addBlock(b) {
    const k = key(b.x, b.y);
    if (blocks.has(k) || coveredByDone(1, b.x, b.y)) return false;
    blocks.set(k, b);
    growBox(b.x, b.y, b.x, b.y);
    notePresence(1, b.x, b.y);
    if (b.count) markBlack(1, b.x, b.y, b.count);
    if (b.count === 64) completeUnit(1, b.x, b.y, null);
    return true;
  }

  /** Create a block only if it doesn't already exist and its section is playable. Never resets existing state. */
  function unlock(x, y, born, hl = 0) {
    if (isOpenArea(x, y) || !blockPlayable(x, y)) return false;
    if (!loading) {
      const t = coarseTargetFor(x, y);
      if (t) return openBigGroup(t.L, t.ux, t.uy, born, hl);
    }
    return addBlock(makeBlock(x, y, 0, 0, born));
  }
  /**
   * Big tiles open 8x8 at a time — the whole unit one level up — just as a 1A shows all its 64 tiles at once.
   * Only untouched children become big tiles; anything already played stays as it is.
   */
  function openBigGroup(L, ux, uy, born, hl) {
    const px = upLevel(ux), py = upLevel(uy), cx0 = px * G - G_OFF, cy0 = py * G - G_OFF;
    bigGroup[L + 1].add(key(px, py));
    let any = false;
    for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
      const cx = cx0 + i, cy = cy0 + j;
      if (!freshUnit(L, cx, cy) || !openCoarse(L, cx, cy, born)) continue;
      any = true;
      if (hl) coarse[L].get(key(cx, cy)).hl = hl;
    }
    return any;
  }

  // Blocks inside a newly finished 1B are dropped a moment later, so their last flips can finish animating.
  const pendingDrop = [];
  let dropTimer = 0;
  function dropSoon(k) {
    pendingDrop.push(k);
    if (!dropTimer) dropTimer = setTimeout(() => {
      dropTimer = 0;
      for (const bk of pendingDrop.splice(0)) {
        const b = blocks.get(bk);
        if (!b || !coveredByDone(1, b.x, b.y)) continue;
        if (b.anim) animCount -= b.anim.size;
        blocks.delete(bk);
      }
      requestRender();
    }, loading ? 0 : 1200);
  }

  /** Forget the per-unit details inside a finished unit; it is now represented by one doneSet entry. */
  function cleanupInside(L, ux, uy) {
    const inside = (x, y, from) => { for (let l = from + 1; l <= L; l++) { x = upLevel(x); y = upLevel(y); } return x === ux && y === uy; };
    const parse = (k) => k.split(',').map(Number);
    if (L === 2) {
      const x0 = unitFirstBlock(ux, 2), y0 = unitFirstBlock(uy, 2);
      for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) if (blocks.has(key(x0 + i, y0 + j))) dropSoon(key(x0 + i, y0 + j));
    } else {
      for (const [k, b] of blocks) if (inside(b.x, b.y, 1)) dropSoon(k);
    }
    for (let j = 2; j < L; j++) {
      for (const m of [black[j], childDone[j], descDone[j]]) for (const k of [...m.keys()]) { const [x, y] = parse(k); if (inside(x, y, j)) m.delete(k); }
      for (const k of [...doneSet[j]]) { const [x, y] = parse(k); if (inside(x, y, j)) doneSet[j].delete(k); }
      for (const k of [...coarse[j].keys()]) { const [x, y] = parse(k); if (inside(x, y, j)) { coarse[j].delete(k); coarseCount--; } }
    }
  }

  /**
   * Finish one unit (block or higher). Every reward happens exactly once: a finished unit is never finished again.
   * Lower units swallowed by a big finish are rewarded too (each 1B inside a finished 1C counts as 1B, and so on).
   * job: the action that caused it (animation / sound bookkeeping), or null while loading.
   */
  function completeUnit(L, ux, uy, job) {
    if (unitComplete(L, ux, uy)) return;
    const k = key(ux, uy);
    let rem, wasBig = false;
    if (L === 1) {
      const b = blocks.get(k);
      if (!b) return; // not open: never finished directly
      rem = 64 - b.count;
      if (rem > 0 && job) animateFill(b, job);
      b.lo = FULL; b.hi = FULL; b.count = 64; b.done = true;
    } else {
      rem = TILES[L] - (black[L].get(k) || 0);
    }
    if (rem > 0) markBlack(L, ux, uy, rem);

    // How many units of each level this adds (itself + everything inside not finished before).
    const newly = new Array(L + 1).fill(0);
    newly[L] = 1;
    if (L >= 2) {
      const dd = descDone[L].get(k);
      for (let j = 1; j < L; j++) newly[j] = Math.pow(64, L - j) - (dd ? dd[j] : 0);
    }
    for (let j = 1; j <= L; j++) cleared[j] += newly[j];
    let ax = ux, ay = uy;
    for (let m = L + 1; m <= MAX_LEVEL; m++) {
      ax = upLevel(ax); ay = upLevel(ay);
      const ak = key(ax, ay);
      let arr = descDone[m].get(ak);
      if (!arr) { arr = new Array(m).fill(0); descDone[m].set(ak, arr); }
      for (let j = 1; j <= L; j++) arr[j] += newly[j];
    }

    if (L >= 2) {
      doneSet[L].add(k);
      if (coarse[L].delete(k)) { coarseCount--; wasBig = true; }
      black[L].delete(k); childDone[L].delete(k); descDone[L].delete(k);
      playCache.clear();
      cleanupInside(L, ux, uy);
      notePresence(L, ux, uy);
      const x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L), span = levelSpan(L);
      growBox(x0, y0, x0 + span - 1, y0 + span - 1);
    }
    if (job) {
      job.completed++;
      if (wasBig) {
        // A big tile turns over just like a tile does (same flip, same wave timing), and has no halo of its own.
        const t = bigTileRect(L, ux, uy);
        if (job.animLeft > 0 && rectInView(t.x, t.y, t.w)) {
          bigAnim.set(L + ':' + k, job.now + Math.min(job.delay, 900));
          job.delay += job.animLeft > 1436 ? job.step : 2;
          job.animLeft--;
        }
      } else if (job.effects < 80) {
        job.effects++;
        if (bigGroup[L].has(k)) {
          // A finished 8x8 of big tiles: the halo a finished 1A gets, scaled up.
          const sc = levelSpan(L);
          effects.push({ ox: unitFirstBlock(ux, L) * PITCH, oy: unitFirstBlock(uy, L) * PITCH, size: sc * PITCH - BLOCK_GAP, t0: job.now + FLIP_MS * 0.5, level: 1, sc });
        } else {
          const x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L), span = levelSpan(L);
          effects.push({ ox: x0 * PITCH, oy: y0 * PITCH, size: span * PITCH - BLOCK_GAP, t0: job.now + FLIP_MS * 0.5, level: L });
        }
      }
    }
    // A big tile opens nothing by itself (its 8x8 group is open already); the group, once finished, opens the next.
    if (!wasBig) openAround(L, ux, uy, job);
    if (L >= ITEM_LEVEL && !loading) itemAfterComplete(L, ux, uy, job);

    // Count it towards the unit above; the 64th child finishes the parent.
    if (L < MAX_LEVEL) {
      const px = upLevel(ux), py = upLevel(uy), pk = key(px, py);
      const n = (childDone[L + 1].get(pk) || 0) + 1;
      childDone[L + 1].set(pk, n);
      if (n >= G_UNITS) completeUnit(L + 1, px, py, job);
    }
  }

  /** Open what a finished unit borders on (only what is playable), and let it breathe softly. */
  function openAround(L, ux, uy, job) {
    if (loading) return;
    const born = job ? job.now + FLIP_MS * 0.6 : 0;
    const hl = job ? job.now + FLIP_MS + 300 : 0;
    const touch = (x, y) => {
      unlock(x, y, born, hl);
      if (!hl) return;
      const n = blocks.get(key(x, y));
      if (n) { if (!n.done) n.hl = hl; return; }
      const c = coarseAt(x, y);
      if (c) { const meta = coarse[c[0]].get(key(c[1], c[2])); if (meta) meta.hl = hl; }
    };
    if (L === 1) { for (const [dx, dy] of NEIGHBOURS) touch(ux + dx, uy + dy); return; }
    const span = levelSpan(L), x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L);
    for (let i = 0; i < span; i++) {
      touch(x0 - 1, y0 + i); touch(x0 + span, y0 + i);
      touch(x0 + i, y0 - 1); touch(x0 + i, y0 + span);
    }
  }

  // ---------- The drag item ----------
  /** The 1C at the middle of a finished unit of level ≥ 3. */
  function itemCOfUnit(L, ux, uy) {
    if (L === ITEM_LEVEL) return [ux, uy];
    const half = levelSpan(L) / 2;
    let x = unitFirstBlock(ux, L) + half, y = unitFirstBlock(uy, L) + half;
    for (let l = 2; l <= ITEM_LEVEL; l++) { x = upLevel(x); y = upLevel(y); }
    return [x, y];
  }
  function itemCenter() {
    const half = levelSpan(ITEM_LEVEL) / 2;
    return { x: (unitFirstBlock(item.x, ITEM_LEVEL) + half) * PITCH - BLOCK_GAP / 2, y: (unitFirstBlock(item.y, ITEM_LEVEL) + half) * PITCH - BLOCK_GAP / 2 };
  }
  function itemAfterComplete(L, ux, uy, job) {
    if (item.s === 1 && unitComplete(ITEM_LEVEL, item.x, item.y)) pickUpItem();
    if (item.s === 0 && cleared[ITEM_LEVEL] >= 2) { const [cx, cy] = itemCOfUnit(L, ux, uy); placeItem(cx, cy, true); }
  }
  /** Put the item in an unfinished 1C next to (cx, cy): random among the four neighbours, else the nearest one further out. */
  function placeItem(cx, cy, live) {
    const pickFrom = (list) => list.length ? list[Math.floor(Math.random() * list.length)] : null;
    let pick = pickFrom(NEIGHBOURS.map(([dx, dy]) => [cx + dx, cy + dy]).filter(([x, y]) => !unitComplete(ITEM_LEVEL, x, y)));
    for (let r = 2; !pick && r < 64; r++) {
      const ring = [];
      for (let i = -r; i <= r; i++) for (const [x, y] of [[cx + i, cy - r], [cx + i, cy + r], [cx - r, cy + i], [cx + r, cy + i]]) if (!unitComplete(ITEM_LEVEL, x, y)) ring.push([x, y]);
      pick = pickFrom(ring);
    }
    if (!pick) return;
    item.s = 1; item.x = pick[0]; item.y = pick[1]; item.t0 = live ? performance.now() : 0;
    itemIntoView();
    scheduleSave();
    requestRender();
  }
  /** Let the furthest zoom-out include the waiting item. */
  function itemIntoView() {
    const x0 = unitFirstBlock(item.x, ITEM_LEVEL), y0 = unitFirstBlock(item.y, ITEM_LEVEL), span = levelSpan(ITEM_LEVEL);
    growBox(x0, y0, x0 + span - 1, y0 + span - 1);
  }
  const itemFx = []; // pick-up rings
  let itemFlying = false;
  function pickUpItem() {
    if (item.s !== 1) return;
    item.s = 2;
    const c = itemCenter(), now = performance.now();
    itemFx.push({ x: c.x, y: c.y, t0: now });
    sound.item();
    flyItemToStatus(c);
    setTimeout(() => showNote(document.body.classList.contains('touch')
      ? 'タイルを押したままなぞると、なぞった所をめくれます'
      : 'タイルを押したままドラッグすると、なぞった所をめくれます', 7000), 1400);
    saveNow();
    requestRender();
  }

  // ---------- Work: one click or one machine action ----------
  // Turns up to C white tiles: the target 1A first, then the nearest open area, widening level by level.
  // Whole units are finished in one step when C covers them, so the cost does not grow with C.
  function newJob(now, vol) {
    return { now, vol, completed: 0, flipped: 0, effects: 0, animLeft: 1500, delay: 0, step: 55, hint: null, hintBlock: null, hintTile: 0 };
  }
  const remaining = (L, ux, uy) => L === 1
    ? 64 - ((blocks.get(key(ux, uy)) || { count: 0 }).count)
    : TILES[L] - (black[L].get(key(ux, uy)) || 0);
  function accessible(L, ux, uy) {
    if (L === 1) { const b = blocks.get(key(ux, uy)); return !!b && !b.done && blockPlayable(ux, uy); }
    return levelUnits[L].has(key(ux, uy));
  }

  function fillUnit(L, ux, uy, C, job) {
    if (C <= 0 || unitComplete(L, ux, uy) || !accessible(L, ux, uy)) return C;
    const rem = remaining(L, ux, uy);
    if (C >= rem) { completeUnit(L, ux, uy, job); return C - rem; }
    if (L === 1) { flipSome(blocks.get(key(ux, uy)), C, job); return 0; }
    if (coarse[L].has(key(ux, uy))) { addProgress(L, ux, uy, C, job); return 0; }
    // Partly. A group of big tiles works like a 1A: from the start tile on, in reading order (wrapping).
    const cx0 = ux * G - G_OFF, cy0 = uy * G - G_OFF, span = levelSpan(L - 1);
    if (bigGroup[L].has(key(ux, uy))) {
      const [hx, hy] = unitAt(L - 1, Math.floor(job.hint.x), Math.floor(job.hint.y));
      let start = -1;
      if (hx >= cx0 && hx < cx0 + G && hy >= cy0 && hy < cy0 + G) start = (hy - cy0) * G + (hx - cx0);
      else {
        let bd = Infinity;
        for (let i = 0; i < G_UNITS; i++) {
          const x = cx0 + (i % G), y = cy0 + ((i / G) | 0);
          if (unitComplete(L - 1, x, y) || !accessible(L - 1, x, y)) continue;
          const dx = unitFirstBlock(x, L - 1) + span / 2 - job.hint.x, dy = unitFirstBlock(y, L - 1) + span / 2 - job.hint.y;
          if (dx * dx + dy * dy < bd) { bd = dx * dx + dy * dy; start = i; }
        }
      }
      if (start < 0) return C;
      for (let k = 0; k < G_UNITS && C > 0; k++) {
        const i = (start + k) % G_UNITS, x = cx0 + (i % G), y = cy0 + ((i / G) | 0);
        if (unitComplete(L - 1, x, y) || !accessible(L - 1, x, y)) continue;
        C = fillUnit(L - 1, x, y, C, job);
      }
      return C;
    }
    // Otherwise: the children nearest to where the action started first.
    for (let guard = 0; guard < G_UNITS && C > 0; guard++) {
      let bx = 0, by = 0, bd = Infinity;
      for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
        const x = cx0 + i, y = cy0 + j;
        if (unitComplete(L - 1, x, y) || !accessible(L - 1, x, y)) continue;
        const dx = unitFirstBlock(x, L - 1) + span / 2 - job.hint.x, dy = unitFirstBlock(y, L - 1) + span / 2 - job.hint.y;
        const d = dx * dx + dy * dy;
        if (d < bd) { bd = d; bx = x; by = y; }
      }
      if (bd === Infinity) break;
      C = fillUnit(L - 1, bx, by, C, job);
    }
    return C;
  }

  /** Part of a big tile turned: it fills up a little (never finishes here — C < what is left). */
  function addProgress(L, ux, uy, C, job) {
    const k = key(ux, uy);
    black[L].set(k, (black[L].get(k) || 0) + C);
    markBlack(L, ux, uy, C);
    job.flipped++;
  }

  /** Index of the tile in b closest to the job's starting point. */
  function nearestTile(b, job) {
    if (b === job.hintBlock) return job.hintTile;
    const col = clamp(Math.floor((job.hint.x - b.x) * N), 0, N - 1), row = clamp(Math.floor((job.hint.y - b.y) * N), 0, N - 1);
    return row * N + col;
  }

  /** Flip C (< whites) tiles of b, from the starting tile on in reading order. */
  function flipSome(b, C, job) {
    const start = nearestTile(b, job);
    for (let k = 0; k < 64 && C > 0; k++) {
      const i = (start + k) % 64;
      if (isBlack(b, i)) continue;
      if (i < 32) b.lo = (b.lo | (1 << i)) >>> 0; else b.hi = (b.hi | (1 << (i - 32))) >>> 0;
      b.count++;
      markBlack(1, b.x, b.y, 1);
      animTile(b, i, job);
      job.flipped++;
      C--;
    }
  }

  function rectInView(x, y, w) {
    const z = cam.z;
    return x + w > cam.x - W / 2 / z && x < cam.x + W / 2 / z && y + w > cam.y - H / 2 / z && y < cam.y + H / 2 / z;
  }
  function inViewBlock(b) {
    const z = cam.z, x = b.x * PITCH, y = b.y * PITCH;
    return x + BLOCK > cam.x - W / 2 / z && x < cam.x + W / 2 / z && y + BLOCK > cam.y - H / 2 / z && y < cam.y + H / 2 / z;
  }
  function animTile(b, i, job) {
    if (job.animLeft <= 0 || !inViewBlock(b)) return;
    if (!b.anim) b.anim = new Map();
    if (!b.anim.has(i)) animCount++;
    b.anim.set(i, job.now + Math.min(job.delay, 900));
    job.delay += job.animLeft > 1436 ? job.step : 2; // a slow wave for the first 64, then quick
    job.animLeft--;
  }
  /** A whole block turning at once: animate its white tiles (starting near the hit), then it is full. */
  function animateFill(b, job) {
    const start = nearestTile(b, job);
    let n = 0;
    for (let k = 0; k < 64; k++) {
      const i = (start + k) % 64;
      if (isBlack(b, i)) continue;
      animTile(b, i, job);
      n++;
    }
    job.flipped += n;
  }

  /** Run an action of C tiles starting at tile i of block b. */
  function work(C, b, i, job) {
    job.hint = { x: b.x + ((i % N) + 0.5) / N, y: b.y + (((i / N) | 0) + 0.5) / N };
    job.hintBlock = b; job.hintTile = i;
    return runWork(C, 1, b.x, b.y, job);
  }
  /** Run an action of C tiles starting on a big tile (level-L unit). */
  function workUnit(C, L, ux, uy, job) {
    const half = levelSpan(L) / 2;
    job.hint = { x: unitFirstBlock(ux, L) + half, y: unitFirstBlock(uy, L) + half };
    return runWork(C, L, ux, uy, job);
  }
  function runWork(C, L0, ux, uy, job) {
    // The wave between flips is paced by how many tiles of the clicked size this action turns
    // (so 4 big tiles ripple exactly like 4 tiles do).
    const n = L0 >= 2 ? C / TILES[L0] : C;
    job.step = Math.min(55, 560 / Math.max(1, Math.min(n, 64) - 1));
    C = fillUnit(L0, ux, uy, C, job);
    for (let L = L0 + 1; L <= MAX_LEVEL && C > 0; L++) {
      ux = upLevel(ux); uy = upLevel(uy);
      C = fillUnit(L, ux, uy, C, job);
    }
    if (job.flipped || job.completed) {
      sound.flip(job.vol);
      if (job.flipped > 1) sound.wave(Math.min(600, job.delay), Math.min(4, job.flipped - 1));
      if (job.completed) { sound.complete(); bumpPoints(); }
      requestUi();
      scheduleSave();
      requestRender();
    }
    return job;
  }

  // ---------- Persistence ----------
  const hex8 = (n) => (n >>> 0).toString(16).padStart(8, '0');
  let saveTimer = 0;

  function saveNow() {
    if (resetting) return;
    clearTimeout(saveTimer);
    saveTimer = 0;
    const list = [];
    for (const b of blocks.values()) if (!coveredByDone(1, b.x, b.y)) list.push([b.x, b.y, b.done ? 'f' : hex8(b.lo) + hex8(b.hi)]);
    const done = {};
    for (let L = 2; L <= MAX_LEVEL; L++) if (doneSet[L].size) done[L] = [...doneSet[L]];
    camTouched = true;
    const data = {
      v: 2, sound: soundOn, hintSeen, theme: themePref, cam: { x: cam.x, y: cam.y, z: cam.z },
      manual: manualLevel,
      spent: spent.map(String),
      bonus: debugBonus.some((v) => v > 0n) ? debugBonus.map(String) : undefined,
      bots: bots.map((b) => ({ t: b.tier, s: b.stage, l: b.legacy ? 1 : 0, x: Math.round(b.x), y: Math.round(b.y) })),
      blocks: list, done,
      item: { s: item.s, x: item.x, y: item.y },
      groups: (() => { const o = {}; for (let P = 3; P <= MAX_LEVEL; P++) if (bigGroup[P].size) o[P] = [...bigGroup[P]]; return o; })(),
      coarse: (() => { const o = {}; for (let L = 2; L <= MAX_LEVEL; L++) if (coarse[L].size) o[L] = [...coarse[L].keys()].map((k) => [k, black[L].get(k) || 0]); return o; })(),
    };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(data)); } catch (_) { /* storage unavailable */ }
  }
  function scheduleSave() {
    if (!saveTimer) saveTimer = setTimeout(saveNow, 400);
  }

  function loadBlocks(list) {
    for (const e of Array.isArray(list) ? list : []) {
      if (!Array.isArray(e) || e.length < 3) continue;
      const x = e[0] | 0, y = e[1] | 0, s = String(e[2]);
      if (s === 'f') addBlock(makeBlock(x, y, FULL, FULL));
      else if (/^[0-9a-f]{16}$/.test(s)) addBlock(makeBlock(x, y, parseInt(s.slice(0, 8), 16), parseInt(s.slice(8), 16)));
    }
  }

  let savedBots = [];
  function load() {
    let raw = null, data = null;
    try { raw = localStorage.getItem(STORAGE_KEY); data = JSON.parse(raw || 'null'); } catch (_) { data = null; }
    loading = true;
    if (data && data.v === 2) {
      loadBlocks(data.blocks);
      if (data.done && typeof data.done === 'object') {
        for (let L = 2; L <= MAX_LEVEL; L++) for (const k of Array.isArray(data.done[L]) ? data.done[L] : []) {
          const [x, y] = String(k).split(',').map(Number);
          if (Number.isInteger(x) && Number.isInteger(y)) completeUnit(L, x, y, null);
        }
      }
      if (data.groups && typeof data.groups === 'object') {
        for (let P = 3; P <= MAX_LEVEL; P++) for (const k of Array.isArray(data.groups[P]) ? data.groups[P] : []) bigGroup[P].add(String(k));
      }
      if (data.coarse && typeof data.coarse === 'object') {
        for (let L = 2; L <= MAX_LEVEL; L++) for (const e of Array.isArray(data.coarse[L]) ? data.coarse[L] : []) {
          if (!Array.isArray(e)) continue;
          const [x, y] = String(e[0]).split(',').map(Number);
          if (!Number.isInteger(x) || !Number.isInteger(y) || !openCoarse(L, x, y, 0)) continue;
          bigGroup[L + 1].add(key(upLevel(x), upLevel(y))); // saves from before groups were stored
          const n = clamp(Number(e[1]) || 0, 0, TILES[L] - 1);
          if (n > 0) { black[L].set(key(x, y), n); markBlack(L, x, y, n); }
        }
      }
      manualLevel = clamp(Number(data.manual) | 0, 0, MANUAL_MAX);
      if (data.item && [0, 1, 2].includes(data.item.s) && Number.isInteger(data.item.x) && Number.isInteger(data.item.y)) {
        item.s = data.item.s; item.x = data.item.x; item.y = data.item.y;
      }
      if (Array.isArray(data.bonus)) data.bonus.forEach((v, L) => { if (L <= MAX_LEVEL && /^\d+$/.test(String(v))) debugBonus[L] = BigInt(v); });
      if (Array.isArray(data.spent)) data.spent.forEach((v, L) => { if (L <= MAX_LEVEL && /^\d+$/.test(String(v))) spent[L] = BigInt(v); });
      savedBots = (Array.isArray(data.bots) ? data.bots : []).filter((b) => b && b.t >= 1 && b.t <= TIERS).map((b) => ({
        tier: b.t | 0, stage: Math.max(0, b.s | 0), legacy: !!b.l, x: Number(b.x) || 0, y: Number(b.y) || 0,
      }));
    } else if (data && data.v === 1 && Array.isArray(data.blocks)) {
      migrateV1(data, raw);
    }
    if (data) {
      soundOn = data.sound !== false;
      hintSeen = !!data.hintSeen;
      themePref = data.theme === 'light' || data.theme === 'dark' ? data.theme : null;
    }
    loading = false;
    if (!isOpenArea(0, 0)) addBlock(makeBlock(0, 0));
    // The item: picked up already if its 1C got finished; saves that are past their 2nd 1C get it placed now.
    if (item.s === 1 && unitComplete(ITEM_LEVEL, item.x, item.y)) item.s = 2;
    if (item.s === 0 && cleared[ITEM_LEVEL] >= 2) {
      let from = null;
      for (let L = MAX_LEVEL; L >= ITEM_LEVEL && !from; L--) for (const k of doneSet[L]) { const [x, y] = k.split(',').map(Number); from = itemCOfUnit(L, x, y); break; }
      if (from) placeItem(from[0], from[1], false);
    }
    if (item.s === 1) itemIntoView();
    // Everything finished opens what it borders on (the save may predate a rule, or be partial).
    for (const b of [...blocks.values()]) if (b.done) openAround(1, b.x, b.y, null);
    for (let L = 2; L <= MAX_LEVEL; L++) for (const k of [...doneSet[L]]) { const [x, y] = k.split(',').map(Number); openAround(L, x, y, null); }
    if (data && data.cam && isFinite(data.cam.x) && isFinite(data.cam.y) && isFinite(data.cam.z)) {
      cam.x = data.cam.x; cam.y = data.cam.y; cam.z = clamp(data.cam.z, minZoom(), MAX_ZOOM);
      camTouched = true;
    } else cam.z = defaultZoom();
  }

  /**
   * Version-1 saves: same board; upgrades were a fixed list and up to 8 identical machines.
   * Points stay exactly as they were (the old purchases are carried over as spent points).
   */
  function migrateV1(data, raw) {
    try { if (raw && !localStorage.getItem(BACKUP_KEY)) localStorage.setItem(BACKUP_KEY, raw); } catch (_) {}
    loadBlocks(data.blocks);
    const u = data.upgrades || {};
    const n = (k) => { const v = u[k]; return v === true ? 1 : Math.max(0, Number(v) | 0); };
    // Manual: the old 2 / 4 / 8 / 64-tile steps are stages 1–4 now, at the same prices.
    manualLevel = n('all64') ? 4 : n('octo') ? 3 : n('quad') ? 2 : n('double') ? 1 : 0;
    spent[1] = BigInt(8 * n('double') + 8 * n('quad') + 48 * n('octo') + 80 * Math.min(4, n('auto')) + 160 * n('botDouble'));
    spent[2] = BigInt(8 * n('all64') + 8 * Math.min(4, n('autoB')));
    // Machines: all become A machines. The first four count towards the new "4 per tier";
    // the extra 8B ones (5th–8th) are kept as migrated machines outside that limit.
    const pos = Array.isArray(data.bots) ? data.bots : (data.bot ? [data.bot] : []);
    const count = Math.min(4, n('auto')) + (n('auto') >= 4 ? Math.min(4, n('autoB')) : 0);
    savedBots = [];
    for (let k = 0; k < count; k++) {
      const p = pos[k] || {};
      savedBots.push({ tier: 1, stage: k < n('botDouble') ? 1 : 0, legacy: k >= 4, x: Number(p.x) || 0, y: Number(p.y) || 0 });
    }
  }

  // ---------- Canvas ----------
  const canvas = document.getElementById('board');
  const ctx = canvas.getContext('2d', { alpha: false });
  let W = window.innerWidth, H = window.innerHeight, DPR = 1;

  function resize() {
    DPR = Math.min(window.devicePixelRatio || 1, 2.5);
    W = window.innerWidth; H = window.innerHeight;
    canvas.width = Math.round(W * DPR);
    canvas.height = Math.round(H * DPR);
    if (!camTouched && W > 0 && H > 0) cam.z = defaultZoom();
    if (cam.z < minZoom()) { cam.z = minZoom(); settleAtMinZoom(); } // window got bigger / smaller
    requestRender();
  }

  /** World rect of every open 1A plus one block of margin on each side (where new ones can open). */
  function worldExtent() {
    const x0 = (bbox.x0 - 1) * PITCH, y0 = (bbox.y0 - 1) * PITCH;
    const x1 = (bbox.x1 + 2) * PITCH - BLOCK_GAP, y1 = (bbox.y1 + 2) * PITCH - BLOCK_GAP;
    return { x0, y0, x1, y1, cx: (x0 + x1) / 2, cy: (y0 + y1) / 2 };
  }
  /** Furthest zoom-out: the whole extent fits the screen with a little room to spare. */
  function minZoom() {
    const w = window.innerWidth || 1, h = window.innerHeight || 1;
    const e = worldExtent();
    return Math.min(MAX_ZOOM, Math.min(w / (e.x1 - e.x0), h / (e.y1 - e.y0)) * 0.9);
  }
  /** When fully zoomed out, centre the view on the extent so everything is really in view. */
  function settleAtMinZoom() {
    if (cam.z > minZoom() * 1.0001) return;
    const e = worldExtent();
    cam.x = e.cx; cam.y = e.cy;
  }

  function defaultZoom() {
    const w = window.innerWidth, h = window.innerHeight;
    return clamp(Math.min(w, h) * 0.62 / BLOCK, minZoom(), MAX_ZOOM);
  }

  const screenToWorld = (sx, sy) => ({ x: (sx - W / 2) / cam.z + cam.x, y: (sy - H / 2) / cam.z + cam.y });

  // ---------- Rendering ----------
  let renderQueued = false;
  function requestRender() {
    if (renderQueued) return;
    renderQueued = true;
    requestAnimationFrame(frame);
  }

  const easeInOut = (t) => t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2;
  const easeOut = (t) => 1 - Math.pow(1 - t, 3);

  function frame(now) {
    renderQueued = false;
    let busy = false;

    if (tween) {
      const t = clamp((now - tween.t0) / tween.dur, 0, 1);
      const e = easeInOut(t);
      cam.x = tween.fx + (tween.tx - tween.fx) * e;
      cam.y = tween.fy + (tween.ty - tween.fy) * e;
      // interpolate zoom geometrically
      cam.z = tween.fz * Math.pow(tween.tz / tween.fz, e);
      if (tween.anchor) {
        // keep a screen anchor fixed while zooming
        const a = tween.anchor;
        cam.x = a.wx - (a.sx - W / 2) / cam.z;
        cam.y = a.wy - (a.sy - H / 2) / cam.z;
      }
      if (t >= 1) { tween = null; scheduleSave(); } else busy = true;
    }

    if (draw(now)) busy = true;
    if (busy) requestRender();
  }

  // Dice faces, in the face's own square (-1…1), for 1–5 points.
  const PIPS = [
    [],
    [[0, 0]],
    [[-1, -1], [1, 1]],
    [[-1, -1], [0, 0], [1, 1]],
    [[-1, -1], [1, -1], [-1, 1], [1, 1]],
    [[-1, -1], [1, -1], [0, 0], [-1, 1], [1, 1]],
  ];

  function draw(now) {
    const z = cam.z;
    bigAnimShown.clear();
    // Retire finished big-tile flips first, so the finished-area pass draws them this very frame (no gap).
    for (const [k, t0] of bigAnim) if (now - t0 >= FLIP_MS) bigAnim.delete(k);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = pal.bg;
    ctx.fillRect(0, 0, canvas.width, canvas.height);

    const s = DPR * z;
    ctx.setTransform(s, 0, 0, s, DPR * (W / 2 - cam.x * z), DPR * (H / 2 - cam.y * z));

    // visible world rect
    const wx0 = cam.x - W / 2 / z, wx1 = cam.x + W / 2 / z;
    const wy0 = cam.y - H / 2 / z, wy1 = cam.y + H / 2 / z;
    const bx0 = Math.floor(wx0 / PITCH), bx1 = Math.floor(wx1 / PITCH);
    const by0 = Math.floor(wy0 / PITCH), by1 = Math.floor(wy1 / PITCH);
    const inView = (x, y) => x >= bx0 && x <= bx1 && y >= by0 && y <= by1;

    const visible = [];
    const area = (bx1 - bx0 + 1) * (by1 - by0 + 1);
    if (area <= blocks.size) {
      for (let by = by0; by <= by1; by++)
        for (let bx = bx0; bx <= bx1; bx++) {
          const b = blocks.get(key(bx, by));
          if (b) visible.push(b);
        }
    } else {
      for (const b of blocks.values())
        if (b.x >= bx0 && b.x <= bx1 && b.y >= by0 && b.y <= by1) visible.push(b);
    }

    let busy = false;
    // Retire finished flips *before* drawing, so a tile is never skipped by both passes
    // (that left a one-frame gap that read as a blink).
    if (animCount > 0) {
      for (const b of blocks.values()) {
        if (!b.anim) continue;
        for (const [i, t0] of b.anim) if (now - t0 >= FLIP_MS) { b.anim.delete(i); animCount--; }
        if (b.anim.size === 0) b.anim = null;
      }
    }
    const screenTile = TILE * z * DPR;
    const coarseView = screenTile < 4;
    const fineDone = screenTile >= 12;   // finished areas: individual tiles only when they're clearly visible
    const useRound = screenTile >= 10 && typeof ctx.roundRect === 'function';
    const r = RADIUS;

    const addRect = useRound
      ? (x, y, w, h, rr) => ctx.roundRect(x, y, w, h, rr)
      : (x, y, w, h) => ctx.rect(x, y, w, h);

    const groupRects = []; // 8x8 groups of big tiles: their separator lines
    // Pass -1: faint outer frames of 1B, 1C, ... units that hold an open block.
    // A level is framed once play spreads over more than one unit of the level below (1B is always framed).
    for (let L = 2; L <= MAX_LEVEL; L++) {
      if (L > 2 && levelUnits[L - 1].size < 2) break;
      const spanW = levelSpan(L) * PITCH;
      const pad = 2.5;
      const size = spanW - BLOCK_GAP + pad * 2;
      ctx.beginPath();
      let any = false;
      const groupLines = [];
      for (const k of levelUnits[L]) {
        if (bigGroup[L].has(k)) { groupLines.push(k); continue; }
        if (coarse[L].has(k)) continue;
        const [ux, uy] = k.split(',').map(Number);
        if (bigGroup[L + 1].has(key(upLevel(ux), upLevel(uy)))) continue;
        const ox = unitFirstBlock(ux, L) * PITCH - pad, oy = unitFirstBlock(uy, L) * PITCH - pad;
        if (ox > wx1 || oy > wy1 || ox + size < wx0 || oy + size < wy0) continue;
        if (ctx.roundRect) ctx.roundRect(ox, oy, size, size, 8 + 4 * L); else ctx.rect(ox, oy, size, size);
        any = true;
      }
      if (any) {
        ctx.strokeStyle = pal.section;
        ctx.lineWidth = (1 + 0.6 * (L - 2)) / (z * DPR) * Math.min(DPR, 1.5);
        ctx.stroke();
      }
      // An 8x8 group of big tiles is marked by a clearer line in the gutter around it (a 1A shows its edge by
      // a wider gap; big tiles stay on their real footprint, so a line does that job instead).
      for (const k of groupLines) {
        const [ux, uy] = k.split(',').map(Number);
        const ox = unitFirstBlock(ux, L) * PITCH - BLOCK_GAP / 2, oy = unitFirstBlock(uy, L) * PITCH - BLOCK_GAP / 2;
        if (ox > wx1 || oy > wy1 || ox + spanW < wx0 || oy + spanW < wy0) continue;
        groupRects.push([ox, oy, spanW, L]); // drawn over the tiles, after pass 1
      }
    }

    // Pass 0: faint 8x8 frames where a block can open next (locked neighbours of open blocks).
    // Neighbours just outside the view can still own a ghost inside it, so scan one block wider.
    const ghosts = new Set();
    const bigGhosts = new Set(); // 8x8 groups of big tiles that open next: 'P:x,y' (the level-P unit holding them)
    const consider = (b) => {
      for (const [dx, dy] of NEIGHBOURS) {
        const gx = b.x + dx, gy = b.y + dy;
        if (!inView(gx, gy) || isOpenArea(gx, gy) || !blockPlayable(gx, gy)) continue;
        const t = coarseTargetFor(gx, gy);
        if (t) bigGhosts.add((t.L + 1) + ':' + upLevel(t.ux) + ',' + upLevel(t.uy)); else ghosts.add(key(gx, gy));
      }
    };
    // Open big tiles: the untouched groups right across their group's edge open next (when their group is done).
    for (let L = 2; L <= MAX_LEVEL; L++) {
      if (!coarse[L].size) continue;
      const span = levelSpan(L);
      for (const k of coarse[L].keys()) {
        const [ux, uy] = k.split(',').map(Number);
        const x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L);
        if (x0 > bx1 + span * G || y0 > by1 + span * G || x0 + span * (G + 1) < bx0 || y0 + span * (G + 1) < by0) continue;
        const px = upLevel(ux), py = upLevel(uy);
        for (const [dx, dy] of NEIGHBOURS) {
          const nx = ux + dx, ny = uy + dy, qx = upLevel(nx), qy = upLevel(ny);
          if (qx === px && qy === py) continue;
          if (freshUnit(L, nx, ny) && freshUnit(L + 1, qx, qy) && unitPlayableAbove(L + 1, qx, qy)) bigGhosts.add((L + 1) + ':' + qx + ',' + qy);
        }
      }
    }
    if ((bx1 - bx0 + 3) * (by1 - by0 + 3) <= blocks.size) {
      for (let by = by0 - 1; by <= by1 + 1; by++)
        for (let bx = bx0 - 1; bx <= bx1 + 1; bx++) {
          const b = blocks.get(key(bx, by));
          if (b && !b.done) consider(b);
        }
    } else {
      for (const b of blocks.values())
        if (!b.done && b.x >= bx0 - 1 && b.x <= bx1 + 1 && b.y >= by0 - 1 && b.y <= by1 + 1) consider(b);
    }
    if (ghosts.size) {
      // Openable blocks: faint outlines of their 64 tiles.
      ctx.beginPath();
      for (const g of ghosts) {
        const [gx, gy] = g.split(',').map(Number);
        const ox = gx * PITCH, oy = gy * PITCH;
        if (coarseView) { ctx.rect(ox, oy, BLOCK, BLOCK); continue; }
        for (let i = 0; i < 64; i++) addRect(ox + (i % N) * CELL, oy + ((i / N) | 0) * CELL, TILE, TILE, r);
      }
      ctx.strokeStyle = pal.ghost;
      ctx.lineWidth = 1 / (z * DPR) * Math.min(DPR, 1.5);
      ctx.stroke();
    }
    if (bigGhosts.size) {
      ctx.beginPath();
      for (const g of bigGhosts) {
        const [P, rest] = g.split(':');
        const L = +P - 1;
        const [px, py] = rest.split(',').map(Number);
        const gx0 = unitFirstBlock(px, +P) * PITCH, gy0 = unitFirstBlock(py, +P) * PITCH, gs = levelSpan(+P) * PITCH;
        if (gx0 > wx1 || gy0 > wy1 || gx0 + gs < wx0 || gy0 + gs < wy0) continue;
        for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
          const cx = px * G - G_OFF + i, cy = py * G - G_OFF + j;
          if (!freshUnit(L, cx, cy)) continue;
          roundRectPath(bigTileDrawRect(L, cx, cy));
        }
      }
      ctx.strokeStyle = pal.ghost;
      ctx.lineWidth = 1 / (z * DPR) * Math.min(DPR, 1.5);
      ctx.stroke();
    }

    // Pass 1: steady tiles, batched by colour. Fading-in blocks drawn separately.
    const fading = [], bigFading = [];
    for (const pass of [0, 1]) {
      ctx.beginPath();
      for (const b of visible) {
        if (b.born && now - b.born < FADE_IN_MS) { if (pass === 0) fading.push(b); continue; }
        drawBlockTiles(b, pass, coarseView, fineDone, addRect, r);
      }
      drawBigTiles(pass, wx0, wx1, wy0, wy1, now, bigFading);
      if (pass === 1) drawDoneUnits(bx0, bx1, by0, by1, wx0, wx1, wy0, wy1, coarseView, fineDone, addRect, r);
      ctx.fillStyle = pass === 0 ? pal.front : pal.back;
      ctx.fill();
    }

    for (const [L, ux, uy, born] of bigFading) {
      ctx.globalAlpha = easeOut(clamp((now - born) / FADE_IN_MS, 0, 1));
      ctx.beginPath();
      roundRectPath(bigTileDrawRect(L, ux, uy));
      ctx.fillStyle = pal.front;
      ctx.fill();
      busy = true;
    }
    ctx.globalAlpha = 1;
    for (const b of fading) {
      const t = (now - b.born) / FADE_IN_MS;
      ctx.globalAlpha = easeOut(clamp(t, 0, 1));
      for (const pass of [0, 1]) {
        ctx.beginPath();
        drawBlockTiles(b, pass, coarseView, fineDone, addRect, r);
        ctx.fillStyle = pass === 0 ? pal.front : pal.back;
        ctx.fill();
      }
      busy = true;
    }
    ctx.globalAlpha = 1;

    // Separator lines of 8x8 groups of big tiles, on top of the tiles so they read clearly at any zoom.
    if (groupRects.length) {
      // Seen from afar (big tiles small on screen) the edge of a group reads like the wider gap between 1A:
      // a background-coloured band about 2.7x the tile gap (BLOCK_GAP : GAP). Up close, a thin clear line.
      const L = groupRects[0][3] - 1;                                   // big-tile level inside the groups
      const tileW = levelSpan(L) * PITCH * z * DPR;                      // big tile on screen (px)
      // From afar the edge tiles are drawn narrower (bigTileDrawRect) — no line needed. Close up, a thin line.
      if (tileW >= 80) {
        ctx.beginPath();
        for (const [ox, oy, gs] of groupRects) ctx.rect(ox, oy, gs, gs);
        ctx.strokeStyle = pal.groupLine;
        ctx.lineWidth = 1.5 / (z * DPR) * Math.min(DPR, 1.5);
        ctx.stroke();
      }
    }

    // Pass 2: flipping tiles. (Too small to see — coarseView — they're simply drawn in their new colour.)
    if (animCount > 0 && !coarseView) {
      for (const b of visible) {
        if (!b.anim) continue;
        const ox = b.x * PITCH, oy = b.y * PITCH;
        for (const [i, t0] of b.anim) {
          const t = (now - t0) / FLIP_MS;
          if (t >= 1) continue;
          const e = easeInOut(clamp(t, 0, 1));
          // Never collapse fully, and blend the colour continuously — no hard steps.
          const sy = 0.12 + 0.88 * Math.abs(Math.cos(Math.PI * e));
          const col = i % N, row = (i / N) | 0;
          const x = ox + col * CELL, y = oy + row * CELL;
          const h = TILE * sy;
          ctx.beginPath();
          addRect(x, y + (TILE - h) / 2, TILE, h, Math.min(r, h / 2));
          const m = e < 0.3 ? 0 : e > 0.7 ? 1 : (e - 0.3) / 0.4;
          const f = pal.frontRgb, k = pal.backRgb;
          ctx.fillStyle = `rgb(${f[0] + (k[0] - f[0]) * m | 0},${f[1] + (k[1] - f[1]) * m | 0},${f[2] + (k[2] - f[2]) * m | 0})`;
          ctx.fill();
        }
      }
      if (animCount > 0) busy = true;
    }

    // Pass 2b: big tiles turning over — the same motion as a tile.
    if (bigAnim.size) {
      for (const [k, t0] of bigAnim) {
        const t = (now - t0) / FLIP_MS;
        if (t >= 1) { bigAnim.delete(k); continue; }
        busy = true;
        const [Ls, rest] = k.split(':');
        const L = +Ls;
        if (!bigAnimShown.has(k)) continue; // drawn as finished area this frame (no hole, no overlay)
        const [ux, uy] = rest.split(',').map(Number);
        const R = bigTileDrawRect(L, ux, uy);
        if (R.x > wx1 || R.y > wy1 || R.x + R.w < wx0 || R.y + R.h < wy0) continue;
        const e = easeInOut(clamp(t, 0, 1));
        const sy = 0.12 + 0.88 * Math.abs(Math.cos(Math.PI * e));
        const h = R.h * sy;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(R.x, R.y + (R.h - h) / 2, R.w, h, Math.min(R.r, h / 2)); else ctx.rect(R.x, R.y + (R.h - h) / 2, R.w, h);
        const m = e < 0.3 ? 0 : e > 0.7 ? 1 : (e - 0.3) / 0.4;
        const f = pal.frontRgb, kk = pal.backRgb;
        ctx.fillStyle = `rgb(${f[0] + (kk[0] - f[0]) * m | 0},${f[1] + (kk[1] - f[1]) * m | 0},${f[2] + (kk[2] - f[2]) * m | 0})`;
        ctx.fill();
      }
    }

    // Pass 3: completion halos.
    for (let k = effects.length - 1; k >= 0; k--) {
      const fx = effects[k];
      const t = (now - fx.t0) / DONE_FX_MS;
      if (t >= 1) { effects.splice(k, 1); continue; }
      busy = true;
      if (t < 0) continue;
      const e = easeOut(t);
      const sc = fx.sc || 1;
      const pad = fx.level > 1 ? (4 + 24 * e) * (fx.level - 1) : (2 + 5 * e) * sc;
      const ox = fx.ox - pad, oy = fx.oy - pad, sz = fx.size + pad * 2;
      if (ox > wx1 || oy > wy1 || ox + sz < wx0 || oy + sz < wy0) continue;
      ctx.beginPath();
      ctx.roundRect ? ctx.roundRect(ox, oy, sz, sz, (fx.level > 1 ? 14 : 6 * sc) + pad * 0.3) : ctx.rect(ox, oy, sz, sz);
      ctx.strokeStyle = `rgba(${pal.halo},${0.22 * (1 - t) * (1 - t)})`;
      ctx.lineWidth = sc > 1 ? 1.5 / z : 1.5 / Math.max(z, 0.3);
      ctx.stroke();
    }

    // Pass 4: blocks opened by a clear breathe softly a few times to show the new directions.
    for (const b of visible) {
      if (!b.hl) continue;
      const t = now - b.hl;
      if (t >= NEW_HL_MS) { b.hl = 0; continue; }
      if (t < 0) { busy = true; continue; }
      const wave = 0.5 - 0.5 * Math.cos((2 * Math.PI * t) / NEW_HL_PERIOD);
      const fade = 1 - t / NEW_HL_MS;
      const pad = 2.5;
      ctx.beginPath();
      ctx.roundRect ? ctx.roundRect(b.x * PITCH - pad, b.y * PITCH - pad, BLOCK + pad * 2, BLOCK + pad * 2, 7) : ctx.rect(b.x * PITCH - pad, b.y * PITCH - pad, BLOCK + pad * 2, BLOCK + pad * 2);
      ctx.strokeStyle = `rgba(${pal.halo},${0.35 * wave * fade})`;
      ctx.lineWidth = 2 / Math.max(z, 0.3);
      ctx.stroke();
      busy = true;
    }

    // Big tiles opened by a clear breathe the same way.
    for (let L = 2; L <= MAX_LEVEL; L++) for (const [k, meta] of coarse[L]) {
      if (!meta.hl) continue;
      const t = now - meta.hl;
      if (t >= NEW_HL_MS) { meta.hl = 0; continue; }
      busy = true;
      if (t < 0) continue;
      const [ux, uy] = k.split(',').map(Number);
      const R = bigTileDrawRect(L, ux, uy), pad = R.w * 0.03;
      if (R.x > wx1 || R.y > wy1 || R.x + R.w < wx0 || R.y + R.h < wy0) continue;
      const wave = 0.5 - 0.5 * Math.cos((2 * Math.PI * t) / NEW_HL_PERIOD);
      ctx.beginPath();
      ctx.roundRect ? ctx.roundRect(R.x - pad, R.y - pad, R.w + pad * 2, R.h + pad * 2, R.r + pad) : ctx.rect(R.x - pad, R.y - pad, R.w + pad * 2, R.h + pad * 2);
      ctx.strokeStyle = `rgba(${pal.halo},${0.35 * wave * (1 - t / NEW_HL_MS)})`;
      ctx.lineWidth = 2 / Math.max(z, 0.02);
      ctx.stroke();
    }

    // Pass 4b: the waiting item, with slow rings; and the rings of a pick-up.
    if (item.s === 1 && drawItem(now, z, wx0, wx1, wy0, wy1)) busy = true;
    for (let k = itemFx.length - 1; k >= 0; k--) {
      const fx = itemFx[k], t = (now - fx.t0) / 1600;
      if (t >= 1) { itemFx.splice(k, 1); continue; }
      busy = true;
      const S = Math.max(150, 46 / z);
      for (let j = 0; j < 4; j++) {
        const tt = clamp(t * 1.25 - j * 0.12, 0, 1);
        if (tt <= 0 || tt >= 1) continue;
        ctx.beginPath();
        ctx.arc(fx.x, fx.y, S * (0.4 + 2.2 * easeOut(tt)), 0, Math.PI * 2);
        ctx.strokeStyle = `rgba(${pal.halo},${0.5 * (1 - tt)})`;
        ctx.lineWidth = (2.2 - j * 0.4) / z;
        ctx.stroke();
      }
      const fl = 1 - t;
      const glow = ctx.createRadialGradient(fx.x, fx.y, 0, fx.x, fx.y, S * 1.2);
      glow.addColorStop(0, `rgba(255,255,255,${0.55 * fl * fl})`);
      glow.addColorStop(1, 'rgba(255,255,255,0)');
      ctx.fillStyle = glow;
      ctx.beginPath(); ctx.arc(fx.x, fx.y, S * 1.2, 0, Math.PI * 2); ctx.fill();
    }

    // Pass 5: machines — silver diamonds with a light face showing dice pips for their tier.
    for (const bot of bots) {
      let x = bot.x, y = bot.y;
      if (bot.glide) {
        const g = bot.glide;
        const e = easeInOut(clamp((now - g.t0) / g.dur, 0, 1));
        x = g.fx + (g.tx - g.fx) * e; y = g.fy + (g.ty - g.fy) * e;
        busy = true;
      }
      // Size follows the work it does: area ∝ tiles per action, so each upgrade makes it √2 bigger
      // (1A per action ≈ the size of a 1A). Never smaller than a few pixels on screen.
      const base = Math.max(TILE * 0.5 * Math.pow(2, botExp(bot) / 2), 6 / (z * DPR));
      if (x < wx0 - base * 1.3 || x > wx1 + base * 1.3 || y < wy0 - base * 1.3 || y > wy1 + base * 1.3) continue;
      const pt = (now - bot.pulse) / 320;
      const pulsing = pt >= 0 && pt < 1;
      const h = base * (pulsing ? 1 + 0.18 * Math.sin(Math.PI * pt) : 1);
      if (pulsing) busy = true;
      drawMachine(x, y, h, bot.tier, z);
    }

    return busy;
  }

  // The cursor-with-motion-lines item, in a unit box (tip at 0,0, about 1 tall).
  const CURSOR = [[0, 0], [0, 0.86], [0.24, 0.66], [0.40, 0.98], [0.55, 0.91], [0.39, 0.60], [0.66, 0.58]];
  const CURSOR_TILT = -0.22;
  function cursorPath(scale, cx, cy) {
    ctx.beginPath();
    CURSOR.forEach(([x, y], k) => {
      const px = cx + (x - cx) * scale, py = cy + (y - cy) * scale;
      if (k) ctx.lineTo(px, py); else ctx.moveTo(px, py);
    });
    ctx.closePath();
  }
  /** Draw the item centred at (x, y), S world units tall. */
  function drawCursorIcon(x, y, S) {
    ctx.save();
    ctx.translate(x, y);
    ctx.rotate(CURSOR_TILT);
    ctx.scale(S, S);
    ctx.translate(-0.33, -0.5);
    // motion lines
    ctx.lineCap = 'round';
    for (const [a, b, c, w] of [[[0.50, 0.93], [0.68, 1.06], [1.02, 1.04], 0.05], [[0.43, 0.99], [0.58, 1.13], [0.86, 1.16], 0.044]]) {
      const g = ctx.createLinearGradient(a[0], a[1], c[0], c[1]);
      g.addColorStop(0, 'rgba(70,70,72,0.95)');
      g.addColorStop(1, 'rgba(140,140,142,0.05)');
      ctx.beginPath(); ctx.moveTo(a[0], a[1]); ctx.quadraticCurveTo(b[0], b[1], c[0], c[1]);
      ctx.strokeStyle = g; ctx.lineWidth = w; ctx.stroke();
    }
    ctx.lineJoin = 'round';
    cursorPath(1, 0.3, 0.55);
    ctx.fillStyle = '#f3f1ec';
    ctx.fill();
    ctx.strokeStyle = '#2b2b2d';
    ctx.lineWidth = 0.06;
    ctx.stroke();
    cursorPath(0.8, 0.29, 0.6);
    ctx.strokeStyle = 'rgba(140,140,138,0.9)';
    ctx.lineWidth = 0.014;
    ctx.stroke();
    ctx.restore();
  }
  /** The waiting item: soft light, slow rings, a gentle float. Returns true while it needs animation frames. */
  function drawItem(now, z, wx0, wx1, wy0, wy1) {
    const c = itemCenter();
    const S = Math.max(150, 46 / z);
    if (c.x + S * 3 < wx0 || c.x - S * 3 > wx1 || c.y + S * 3 < wy0 || c.y - S * 3 > wy1) return false;
    const t = now / 1000;
    const appear = item.t0 ? clamp((now - item.t0) / 1200, 0, 1) : 1;
    ctx.globalAlpha = easeOut(appear);
    const glow = ctx.createRadialGradient(c.x, c.y, 0, c.x, c.y, S * 1.3);
    glow.addColorStop(0, 'rgba(255,255,255,0.75)');
    glow.addColorStop(0.55, 'rgba(255,255,255,0.25)');
    glow.addColorStop(1, 'rgba(255,255,255,0)');
    ctx.fillStyle = glow;
    ctx.beginPath(); ctx.arc(c.x, c.y, S * 1.3, 0, Math.PI * 2); ctx.fill();
    for (let j = 0; j < 3; j++) {
      const ph = ((t / 2.8) + j / 3) % 1;
      ctx.beginPath();
      ctx.arc(c.x, c.y, S * (0.55 + 1.15 * ph), 0, Math.PI * 2);
      ctx.strokeStyle = `rgba(${pal.halo},${0.32 * Math.sin(Math.PI * ph)})`;
      ctx.lineWidth = 1.6 / z;
      ctx.stroke();
    }
    ctx.beginPath();
    ctx.arc(c.x, c.y, S * 0.62, 0, Math.PI * 2);
    ctx.strokeStyle = `rgba(${pal.halo},0.22)`;
    ctx.lineWidth = 1 / z;
    ctx.stroke();
    drawCursorIcon(c.x, c.y + Math.sin(t * 1.6) * S * 0.04, S * 0.78);
    ctx.globalAlpha = 1;
    return true;
  }

  function diamondPath(x, y, h) {
    ctx.beginPath();
    ctx.moveTo(x, y - h); ctx.lineTo(x + h, y); ctx.lineTo(x, y + h); ctx.lineTo(x - h, y);
    ctx.closePath();
  }
  function drawMachine(x, y, h, tier, z) {
    // Brushed-silver body: a diagonal gradient with a bright band, two soft facets and a thin edge.
    const grad = ctx.createLinearGradient(x - h, y - h, x + h, y + h);
    grad.addColorStop(0, '#fbfbfc');
    grad.addColorStop(0.28, '#c3c7ce');
    grad.addColorStop(0.46, '#f2f3f5');
    grad.addColorStop(0.62, '#9da3ac');
    grad.addColorStop(0.82, '#d9dce1');
    grad.addColorStop(1, '#7f858f');
    diamondPath(x, y, h);
    ctx.fillStyle = grad;
    ctx.fill();
    ctx.beginPath();
    ctx.moveTo(x, y - h); ctx.lineTo(x + h, y); ctx.lineTo(x, y + h); ctx.closePath();
    ctx.fillStyle = 'rgba(40,44,52,0.10)';
    ctx.fill();
    diamondPath(x, y, h);
    ctx.strokeStyle = pal.botEdge;
    ctx.lineWidth = 1.1 / Math.max(z, 0.4);
    ctx.stroke();
    // Light face for the pips.
    const f = h * 0.74;
    const face = ctx.createLinearGradient(x - f, y - f, x + f, y + f);
    face.addColorStop(0, '#ffffff');
    face.addColorStop(1, '#e4e6ea');
    diamondPath(x, y, f);
    ctx.fillStyle = face;
    ctx.fill();
    ctx.strokeStyle = 'rgba(90,96,106,0.35)';
    ctx.lineWidth = 0.8 / Math.max(z, 0.4);
    ctx.stroke();
    // Pips: dice layout turned 45° with the face.
    const sp = f * 0.3, pr = h * 0.095;
    ctx.beginPath();
    for (const [u, v] of PIPS[tier] || []) {
      const px = x + (u - v) * sp, py = y + (u + v) * sp;
      ctx.moveTo(px + pr, py);
      ctx.arc(px, py, pr, 0, Math.PI * 2);
    }
    ctx.fillStyle = '#1e2024';
    ctx.fill();
  }

  function blockTilesAll(ox, oy, addRect, r) {
    for (let i = 0; i < 64; i++) addRect(ox + (i % N) * CELL, oy + ((i / N) | 0) * CELL, TILE, TILE, r);
  }

  function drawBlockTiles(b, pass, coarse, fineDone, addRect, r) {
    const ox = b.x * PITCH, oy = b.y * PITCH;
    const wantBlack = pass === 1;
    if (b.done && !fineDone) { if (wantBlack && !b.anim) addRect(ox, oy, BLOCK, BLOCK, r * 2); if (!b.anim) return; }
    if (b.count === 0) {
      if (wantBlack) return;
      if (coarse) { addRect(ox, oy, BLOCK, BLOCK, r * 2); return; }
    }
    if (coarse) {
      // Too small to see tiles: draw per-cell rects.
      for (let row = 0; row < N; row++) {
        for (let col = 0; col < N; col++) {
          const i = row * N + col;
          if (isBlack(b, i) === wantBlack) ctx.rect(ox + col * CELL, oy + row * CELL, CELL, CELL);
        }
      }
      return;
    }
    const anim = b.anim;
    for (let i = 0; i < 64; i++) {
      if (anim && anim.has(i)) continue;
      if (isBlack(b, i) !== wantBlack) continue;
      const col = i % N, row = (i / N) | 0;
      addRect(ox + col * CELL, oy + row * CELL, TILE, TILE, r);
    }
  }

  /** A level-L unit drawn as one tile: inset like a tile in its cell, corners rounded like a tile. */
  function bigTileRect(L, ux, uy) {
    // Exactly on its own 1B (or 1D) footprint, so it always lines up with the real tiles around it;
    // inset like a tile in its cell (gap ≈ GAP / CELL), corners rounded like a tile.
    const size = levelSpan(L) * PITCH - BLOCK_GAP, m = size * (GAP / CELL) / 2;
    return { x: unitFirstBlock(ux, L) * PITCH + m, y: unitFirstBlock(uy, L) * PITCH + m, w: size - 2 * m, r: (size - 2 * m) * (RADIUS / TILE) };
  }
  /**
   * Where a big tile is drawn. Seen from afar, the tiles on the outer edge of an 8x8 group are drawn a little
   * narrower on that edge, so the gap between groups reads like the wider gap between 1A (BLOCK_GAP : GAP) —
   * still a rounded tile, still inside its own footprint, so nothing shifts against the real tiles.
   */
  function bigTileDrawRect(L, ux, uy) {
    const t = bigTileRect(L, ux, uy);
    const px = upLevel(ux), py = upLevel(uy);
    const e = bigGroup[L + 1].has(key(px, py)) ? groupEdgeInset(L) : 0;
    if (!e) return { x: t.x, y: t.y, w: t.w, h: t.w, r: t.r };
    const ix = ux - (px * G - G_OFF), iy = uy - (py * G - G_OFF);
    let x = t.x, y = t.y, w = t.w, h = t.w;
    if (ix === 0) { x += e; w -= e; }
    if (ix === G - 1) w -= e;
    if (iy === 0) { y += e; h -= e; }
    if (iy === G - 1) h -= e;
    return { x, y, w, h, r: Math.min(t.r, w / 2, h / 2) };
  }
  /** Extra inset (world units) for group-edge big tiles at the current zoom; 0 when close up. */
  function groupEdgeInset(L) {
    const tileW = levelSpan(L) * PITCH * cam.z * DPR;
    if (tileW >= 80) return 0;
    const gapPx = tileW * (GAP / CELL);
    const want = Math.max(2, gapPx * (BLOCK_GAP / GAP));
    return Math.max(0, (want - gapPx) / 2) / (cam.z * DPR);
  }
  const roundRectPath = (t) => { if (ctx.roundRect) ctx.roundRect(t.x, t.y, t.w, t.h, t.r); else ctx.rect(t.x, t.y, t.w, t.h); };

  /** Big tiles: white face (pass 0); progress as a black square growing from the middle (pass 1). */
  function drawBigTiles(pass, wx0, wx1, wy0, wy1, now, fadingOut) {
    if (!coarseCount) return;
    for (let L = 2; L <= MAX_LEVEL; L++) {
      for (const [k, meta] of coarse[L]) {
        const [ux, uy] = k.split(',').map(Number);
        const t = bigTileDrawRect(L, ux, uy);
        if (t.x > wx1 || t.y > wy1 || t.x + t.w < wx0 || t.y + t.h < wy0) continue;
        if (meta.born && now - meta.born < FADE_IN_MS) { if (pass === 0) fadingOut.push([L, ux, uy, meta.born]); continue; }
        if (pass === 0) { roundRectPath(t); continue; }
        const p = (black[L].get(k) || 0) / TILES[L];
        if (p <= 0) continue;
        const side = Math.min(t.w, t.h) * Math.sqrt(p), ox = (t.w - side) / 2, oy = (t.h - side) / 2;
        if (ctx.roundRect) ctx.roundRect(t.x + ox, t.y + oy, side, side, t.r * Math.sqrt(p)); else ctx.rect(t.x + ox, t.y + oy, side, side);
      }
    }
  }

  /**
   * Finished 1B / 1C / ... areas (no per-block data left): black, drawn only where visible, at the finest grain
   * that is still readable on screen (tiles, 1A, 1B, ...) so they keep the look of a tiled board.
   */
  function drawDoneUnits(bx0, bx1, by0, by1, wx0, wx1, wy0, wy1, coarseView, fineDone, addRect, r) {
    const px = PITCH * cam.z * DPR;
    for (let L = 2; L <= MAX_LEVEL; L++) {
      if (!doneSet[L].size) continue;
      const span = levelSpan(L);
      let sl = 1;                                         // drawing grain
      while (sl < L && levelSpan(sl) * px < 10) sl++;
      for (const k of doneSet[L]) {
        const [ux, uy] = k.split(',').map(Number);
        const x0 = unitFirstBlock(ux, L), y0 = unitFirstBlock(uy, L);
        if (x0 > bx1 || y0 > by1 || x0 + span - 1 < bx0 || y0 + span - 1 < by0) continue;
        if (sl === 1) {
          const xa = Math.max(x0, bx0), xb = Math.min(x0 + span - 1, bx1);
          const ya = Math.max(y0, by0), yb = Math.min(y0 + span - 1, by1);
          for (let y = ya; y <= yb; y++) for (let x = xa; x <= xb; x++) {
            if (blocks.has(key(x, y))) continue; // still fading out of the block list: drawn there
            if (bigAnim.size) {
              // Inside a big tile that is still turning over: its flip is drawn in its place.
              const [qx, qy] = unitAt(2, x, y), ak = '2:' + qx + ',' + qy;
              if (bigAnim.has(ak)) { bigAnimShown.add(ak); continue; }
            }
            if (fineDone) blockTilesAll(x * PITCH, y * PITCH, addRect, r);
            else addRect(x * PITCH, y * PITCH, BLOCK, BLOCK, r * 2);
          }
          continue;
        }
        const g = Math.max(sl, 2);                         // draw level-g sub-units as tiles
        const roundBig = levelSpan(g) * px >= 10;
        const [ga, gb] = [unitAt(g, Math.max(x0, bx0), 0)[0], unitAt(g, Math.min(x0 + span - 1, bx1), 0)[0]];
        const [ha, hb] = [unitAt(g, 0, Math.max(y0, by0))[1], unitAt(g, 0, Math.min(y0 + span - 1, by1))[1]];
        for (let y = ha; y <= hb; y++) for (let x = ga; x <= gb; x++) {
          if (bigAnim.size) {
            const ak = g + ':' + x + ',' + y;
            if (bigAnim.has(ak)) { bigAnimShown.add(ak); continue; } // its flip is drawn in its place
          }
          const t = bigTileDrawRect(g, x, y);
          // Same rounded corners as the open big tiles, once they're big enough on screen to show them.
          if (roundBig) roundRectPath(t); else ctx.rect(t.x, t.y, t.w, t.h);
        }
      }
    }
  }

  // ---------- Game logic ----------
  /** Returns {b, i} for the tile of an open, unfinished-area block under a world point, or null. */
  function tileAt(wx, wy) {
    const bx = Math.floor(wx / PITCH), by = Math.floor(wy / PITCH);
    const lx = wx - bx * PITCH, ly = wy - by * PITCH;
    if (lx >= BLOCK || ly >= BLOCK) return null;
    const b = blocks.get(key(bx, by));
    if (!b) return null;
    const col = Math.min(N - 1, Math.floor(lx / CELL));
    const row = Math.min(N - 1, Math.floor(ly / CELL));
    return { b, i: row * N + col };
  }
  /** The unfinished big tile under a world point, as [L, ux, uy], or null. */
  function bigTileAt(wx, wy) {
    const c = coarseAt(Math.floor(wx / PITCH), Math.floor(wy / PITCH));
    return c && !unitComplete(c[0], c[1], c[2]) ? c : null;
  }
  /** Is there a tile (open or finished) under a world point? Used to decide click vs. pan. */
  function isTileArea(wx, wy) {
    const bx = Math.floor(wx / PITCH), by = Math.floor(wy / PITCH);
    if (coarseAt(bx, by)) return true;
    const lx = wx - bx * PITCH, ly = wy - by * PITCH;
    if (lx >= BLOCK || ly >= BLOCK) return false;
    return isOpenArea(bx, by);
  }

  // ---------- UI ----------
  const pointsEl = document.getElementById('points');
  const soundBtn = document.getElementById('btn-sound');
  const hintEl = document.getElementById('hint');
  const itemIconEl = document.getElementById('item-icon');
  const CURSOR_SVG = (() => {
    const pts = CURSOR.map(([x, y]) => `${x},${y}`).join(' ');
    const inner = CURSOR.map(([x, y]) => `${0.29 + (x - 0.29) * 0.8},${0.6 + (y - 0.6) * 0.8}`).join(' ');
    return `<svg viewBox="-0.25 -0.2 1.5 1.5" aria-hidden="true"><g transform="rotate(${CURSOR_TILT * 180 / Math.PI} 0.33 0.5)">`
      + `<path d="M0.50 0.93 Q0.68 1.06 1.02 1.04" stroke="#6a6a6c" stroke-width="0.05" fill="none" stroke-linecap="round" opacity="0.8"/>`
      + `<path d="M0.43 0.99 Q0.58 1.13 0.86 1.16" stroke="#6a6a6c" stroke-width="0.044" fill="none" stroke-linecap="round" opacity="0.7"/>`
      + `<polygon points="${pts}" fill="#f3f1ec" stroke="#2b2b2d" stroke-width="0.06" stroke-linejoin="round"/>`
      + `<polygon points="${inner}" fill="none" stroke="#8c8c8a" stroke-width="0.014" stroke-linejoin="round"/></g></svg>`;
  })();
  itemIconEl.innerHTML = CURSOR_SVG;
  /** After pick-up: the item glides from the board to the points box and stays there as a small icon. */
  function flyItemToStatus(c) {
    itemFlying = true;
    const sx = clamp((c.x - cam.x) * cam.z + W / 2, 40, W - 40), sy = clamp((c.y - cam.y) * cam.z + H / 2, 40, H - 40);
    const fly = document.createElement('div');
    fly.className = 'item-fly';
    fly.innerHTML = CURSOR_SVG;
    fly.style.left = (sx - 32) + 'px';
    fly.style.top = (sy - 32) + 'px';
    document.body.append(fly);
    itemIconEl.hidden = false;
    itemIconEl.style.visibility = 'hidden';
    const r = itemIconEl.getBoundingClientRect();
    const tx = r.left + r.width / 2 - sx, ty = r.top + r.height / 2 - sy;
    setTimeout(() => {
      fly.style.transform = `translate(${tx}px, ${ty}px) scale(0.3)`;
      fly.style.opacity = '0.9';
    }, 650);
    setTimeout(() => {
      fly.remove();
      itemFlying = false;
      itemIconEl.style.visibility = '';
      itemIconEl.classList.add('pop');
      setTimeout(() => itemIconEl.classList.remove('pop'), 700);
    }, 650 + 1000);
  }
  /** A short line of text at the bottom, using the hint spot. */
  function showNote(text, ms) {
    const keep = hintEl.innerHTML;
    hintEl.textContent = text;
    hintEl.hidden = false;
    requestAnimationFrame(() => hintEl.classList.add('show'));
    setTimeout(() => {
      hintEl.classList.remove('show');
      setTimeout(() => { hintEl.hidden = true; hintEl.innerHTML = keep; }, 1300);
    }, ms);
  }
  const shopBtn = document.getElementById('btn-shop');
  const shopEl = document.getElementById('shop');
  let bumpTimer = 0;

  let uiQueued = false;
  function requestUi() {
    if (uiQueued) return;
    uiQueued = true;
    requestAnimationFrame(() => { uiQueued = false; updatePoints(); renderShop(); });
  }
  function bumpPoints() {
    pointsEl.classList.add('bump');
    clearTimeout(bumpTimer);
    bumpTimer = setTimeout(() => pointsEl.classList.remove('bump'), 1400);
  }
  function updatePoints() {
    // Highest level first, e.g. "1B 12A": points left to spend at each level reached. Never converted.
    const parts = [];
    for (let L = MAX_LEVEL; L >= 2; L--) if (cleared[L] > 0 || debugBonus[L] > 0n) parts.push(fmt(avail(L)) + letter(L));
    if (!itemFlying) itemIconEl.hidden = item.s !== 2;
    parts.push(fmt(avail(1)) + 'A');
    pointsEl.textContent = parts.join(' ');
  }

  // Shop panel: manual stage, then one section per machine tier.
  const tierUnlocked = (t) => t === 1 || cleared[t] > 0 || debugBonus[t] > 0n;
  const tierBots = (t) => bots.filter((b) => b.tier === t);
  const tierCount = (t) => bots.filter((b) => b.tier === t && !b.legacy).length;
  let shopOpen = false, shopSig = '';
  const refs = new Map(); // row id → { name, desc, btn, why }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function row(id, act, data) {
    const r = el('div', 'shop-row');
    const name = el('div', 'shop-name'), desc = el('div', 'shop-desc');
    const btn = el('button', 'shop-btn');
    btn.type = 'button';
    btn.dataset.act = act;
    for (const [k, v] of Object.entries(data || {})) btn.dataset[k] = v;
    const side = el('div', 'shop-side');
    side.append(btn);
    r.append(name, desc, side);
    refs.set(id, { row: r, name, desc, btn });
    return r;
  }
  const setText = (e, t) => { if (e.textContent !== t) e.textContent = t; };
  /** Buttons only dim when unaffordable (no extra wording); rows that hit their limit are hidden. */
  function setBtn(ref, label, ok) {
    setText(ref.btn, label);
    ref.btn.disabled = !ok;
  }
  const setShown = (e, on) => { if (e.hidden === on) e.hidden = !on; };

  function renderShop() {
    if (!shopOpen) return;
    const tiers = [];
    for (let t = 1; t <= TIERS; t++) if (tierUnlocked(t)) tiers.push(t);
    const sig = tiers.join(',') + '|' + bots.map((b) => b.id).join(',');
    if (sig !== shopSig) {
      shopSig = sig;
      refs.clear();
      const parts = [];
      const man = el('section', 'shop-sec');
      man.append(el('h3', 'shop-h', '手動'), row('manual', 'manual'));
      refs.set('manualSec', { row: man });
      parts.push(man);
      for (const t of tiers) {
        const sec = el('section', 'shop-sec');
        const h = el('h3', 'shop-h');
        h.append(el('span', '', letter(t) + '機械 '), el('span', 'shop-dots', '●'.repeat(t)));
        const meta = el('div', 'shop-meta');
        refs.set('meta' + t, { meta });
        sec.append(h, meta, row('buy' + t, 'buy', { tier: String(t) }));
        for (const b of tierBots(t)) sec.append(row('bot' + b.id, 'up', { bot: String(b.id) }));
        parts.push(sec);
      }
      shopEl.replaceChildren(...parts);
    }
    // Manual
    const m = refs.get('manual');
    setShown(refs.get('manualSec').row, manualLevel < MANUAL_MAX); // all manual stages bought: hide
    if (manualLevel < MANUAL_MAX) {
      const p = manualPrice(manualLevel);
      setText(m.name, '1回で返す量');
      setText(m.desc, `現在：${capLabel(manualExp(manualLevel))}／回 → 次：${capLabel(manualExp(manualLevel + 1))}／回`);
      setBtn(m, `強化 ${fmt(p.cost)}${letter(p.unit)}`, canPay(p.unit, p.cost));
    }
    for (const t of tiers) {
      const L = letter(t), own = tierCount(t), legacy = tierBots(t).length - own;
      setText(refs.get('meta' + t).meta, `所有 ${own}/${MACHINES_PER_TIER}台${legacy ? `（移行分 +${legacy}台）` : ''}　所持 ${fmt(avail(t))}${L}`);
      const buy = refs.get('buy' + t);
      setText(buy.name, '本体');
      setText(buy.desc, `${fmt(MACHINE_PRICE)}${L}／台　購入時 ${capLabel(6 * (t - 1))}／動作`);
      setShown(buy.row, own < MACHINES_PER_TIER); // 4 bought: no more buying, hide the row
      setBtn(buy, `購入 ${fmt(MACHINE_PRICE)}${L}`, canPay(t, MACHINE_PRICE));
      let no = 0;
      for (const b of tierBots(t)) {
        no++;
        const ref = refs.get('bot' + b.id);
        const cost = upgradeCost(b.stage);
        setText(ref.name, `${L}機械 ${no}号${b.legacy ? '（移行）' : ''}`);
        setText(ref.desc, `現在：${capLabel(botExp(b))}／動作 → 次：${capLabel(botExp(b) + 1)}／動作`);
        setBtn(ref, `強化 ${fmt(cost)}${L}`, canPay(t, cost));
      }
    }
  }

  function setShop(open) {
    shopOpen = open;
    shopEl.hidden = !open;
    shopBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
    if (open) { shopSig = ''; renderShop(); }
  }
  shopBtn.addEventListener('click', () => { setShop(!shopOpen); shopBtn.blur(); });
  shopEl.addEventListener('pointerdown', () => sound.unlock());
  shopEl.addEventListener('click', (e) => {
    const btn = e.target.closest('.shop-btn');
    if (!btn || btn.disabled) return;
    btn.blur();
    if (buy(btn.dataset.act, btn.dataset)) {
      sound.complete();
      requestUi();
      saveNow();
    }
  });

  /** All purchases go through here. Each pays only with the points of its own tier. */
  function buy(act, d) {
    if (act === 'manual') {
      if (manualLevel >= MANUAL_MAX) return false;
      const p = manualPrice(manualLevel);
      if (!pay(p.unit, p.cost)) return false;
      manualLevel++;
      return true;
    }
    if (act === 'buy') {
      const t = Number(d.tier);
      if (!(t >= 1 && t <= TIERS) || !tierUnlocked(t) || tierCount(t) >= MACHINES_PER_TIER) return false;
      if (!pay(t, MACHINE_PRICE)) return false;
      addBot({ tier: t, stage: 0, legacy: false }, true);
      return true;
    }
    if (act === 'up') {
      const b = bots.find((x) => x.id === Number(d.bot));
      if (!b) return false;
      if (!pay(b.tier, upgradeCost(b.stage))) return false;
      b.stage++;
      return true;
    }
    return false;
  }

  const themeBtn = document.getElementById('btn-theme');
  const darkQuery = matchMedia('(prefers-color-scheme: dark)');
  const currentTheme = () => themePref || (darkQuery.matches ? 'dark' : 'light');
  function applyTheme() {
    const t = currentTheme();
    pal = PALETTES[t];
    document.documentElement.dataset.theme = t;
    themeBtn.setAttribute('aria-pressed', t === 'dark' ? 'true' : 'false');
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.content = pal.bg;
    requestRender();
  }
  themeBtn.addEventListener('click', () => {
    themePref = currentTheme() === 'dark' ? 'light' : 'dark';
    applyTheme();
    saveNow();
  });
  darkQuery.addEventListener('change', () => { if (!themePref) applyTheme(); });

  function updateSoundBtn() { soundBtn.setAttribute('aria-pressed', soundOn ? 'true' : 'false'); }

  function zoomBy(factor, sx = W / 2, sy = H / 2, animate = true) {
    const mz = minZoom();
    const tz = clamp((tween ? tween.tz : cam.z) * factor, mz, MAX_ZOOM); // chain from the zoom already under way
    const p = screenToWorld(sx, sy);
    if (!animate) {
      tween = null;
      cam.z = tz;
      cam.x = p.x - (sx - W / 2) / cam.z;
      cam.y = p.y - (sy - H / 2) / cam.z;
      settleAtMinZoom();
      scheduleSave();
      requestRender();
      return;
    }
    if (tz <= mz * 1.0001) {
      // Reaching the furthest zoom-out: glide to show the whole board.
      const e = worldExtent();
      tween = { t0: performance.now(), dur: 320, fx: cam.x, fy: cam.y, fz: cam.z, tx: e.cx, ty: e.cy, tz, anchor: null };
      requestRender();
      return;
    }
    tween = { t0: performance.now(), dur: 220, fx: cam.x, fy: cam.y, fz: cam.z, tx: cam.x, ty: cam.y, tz, anchor: { wx: p.x, wy: p.y, sx, sy } };
    requestRender();
  }

  function goHome() {
    tween = { t0: performance.now(), dur: 520, fx: cam.x, fy: cam.y, fz: cam.z, tx: BLOCK / 2, ty: BLOCK / 2, tz: defaultZoom(), anchor: null };
    requestRender();
  }

  // ---------- Debug panel (top right) ----------
  // Everything here is for testing: jumps to set-up states, extra points, machine controls, save copy/paste.
  const debugBtn = document.getElementById('btn-debug');
  const debugEl = document.getElementById('debug-panel');
  let debugOpen = false, debugTimer = 0;

  /** Replace the save with `data` (keeping sound / theme) and reload. */
  function jumpTo(data) {
    const keep = { v: 2, sound: soundOn, theme: themePref, hintSeen: true };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(Object.assign(keep, data))); } catch (_) {}
    resetting = true; // block the pagehide save from writing the old state back
    location.reload();
  }
  /** Just before picking up the drag item: 1C (0,0) and (1,0) finished, the item waits in 1C (2,0),
      which is finished except one white tile in 1A (92, 0) — the view opens on it. */
  function itemReadyState() {
    const IC = { x: 2, y: 0 }, lastB = { x: 12, y: 0 }, lastA = { x: 92, y: 0 }, lastTile = 27;
    const bx0 = IC.x * G - G_OFF, by0 = IC.y * G - G_OFF;
    const doneB = [];
    for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
      const x = bx0 + i, y = by0 + j;
      if (x !== lastB.x || y !== lastB.y) doneB.push(key(x, y));
    }
    const ax0 = unitFirstBlock(lastB.x, 2), ay0 = unitFirstBlock(lastB.y, 2);
    const list = [];
    for (let j = 0; j < G; j++) for (let i = 0; i < G; i++) {
      const x = ax0 + i, y = ay0 + j;
      if (x === lastA.x && y === lastA.y) {
        const lo = lastTile < 32 ? (FULL & ~(1 << lastTile)) >>> 0 : FULL;
        const hi = lastTile >= 32 ? (FULL & ~(1 << (lastTile - 32))) >>> 0 : FULL;
        list.push([x, y, hex8(lo) + hex8(hi)]);
      } else list.push([x, y, 'f']);
    }
    return {
      cam: { x: lastA.x * PITCH + BLOCK / 2, y: lastA.y * PITCH + BLOCK / 2, z: 1.2 },
      blocks: list, done: { 2: doneB, 3: ['0,0', '1,0'] }, item: { s: 1, x: IC.x, y: IC.y },
    };
  }

  const DEBUG_ACTIONS = {
    reset: { ask: '進行状況をリセットしますか？', run: () => jumpTo({ blocks: [] }) },
    jumpItem: { ask: 'リセットして、アイテム取得の直前まで進めますか？', run: () => jumpTo(itemReadyState()) },
    jumpBig: { ask: 'リセットして、最初の1Dを完成させた直後（大タイルが開く所）まで進めますか？',
      run: () => jumpTo({ blocks: [], done: { 4: ['0,0'] }, cam: { x: 0, y: 0, z: 0.004 } }) },
    clearBonus: { run: () => { debugBonus.fill(0n); } },
    manualMax: { run: () => { manualLevel = MANUAL_MAX; } },
    manualZero: { run: () => { manualLevel = 0; } },
    pause: { run: () => { botPaused = !botPaused; } },
    speed: { run: () => { botSpeed = botSpeed === 1 ? 10 : 1; } },
    itemGet: { run: () => { if (item.s === 1) pickUpItem(); else if (item.s === 0) { item.s = 2; } } },
    itemReset: { run: () => { item.s = 0; itemIconEl.hidden = true; } },
    copySave: { run: () => {
      saveNow();
      const text = localStorage.getItem(STORAGE_KEY) || '';
      const done = () => showNote('セーブデータをコピーしました', 2500);
      if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(text).then(done, () => prompt('コピーしてください', text));
      else prompt('コピーしてください', text);
    } },
    pasteSave: { run: () => {
      const text = prompt('セーブデータ（JSON）を貼り付けてください');
      if (!text) return;
      let data = null;
      try { data = JSON.parse(text); } catch (_) {}
      if (!data || (data.v !== 1 && data.v !== 2)) { alert('読み込めないデータです'); return; }
      if (!confirm('今の進行状況をこのデータで置き換えますか？')) return;
      try { localStorage.setItem(STORAGE_KEY, text); } catch (_) {}
      resetting = true;
      location.reload();
    } },
  };

  function renderDebug() {
    if (!debugOpen) return;
    const tiers = [1, 2, 3, 4, 5];
    const bonus = tiers.map((t) => `<div class="dbg-row"><span class="dbg-label">${letter(t)}</span>`
      + `<button data-grant="${t}:100">+100</button><button data-grant="${t}:10000">+1万</button><button data-grant="${t}:100000000">+1億</button>`
      + `<span class="dbg-val">${fmt(avail(t))}${letter(t)}</span></div>`).join('');
    const doneCounts = [];
    for (let L = 2; L <= 6; L++) if (doneSet[L].size) doneCounts.push(`${letter(L)}:${doneSet[L].size}`);
    debugEl.innerHTML = ''
      + `<section><h3>進行</h3><div class="dbg-row">`
      + `<button data-act="reset">リセット</button><button data-act="jumpItem">アイテム直前</button><button data-act="jumpBig">1D完成直後</button></div></section>`
      + `<section><h3>ポイント（デバッグ用に別枠で加算・保存）</h3>${bonus}`
      + `<div class="dbg-row"><button data-act="clearBonus">加算分を消す</button></div></section>`
      + `<section><h3>手動</h3><div class="dbg-row"><span class="dbg-val">${capLabel(manualExp(manualLevel))}／回</span>`
      + `<button data-act="manualMax">最大にする</button><button data-act="manualZero">強化なしに戻す</button></div></section>`
      + `<section><h3>機械（${bots.length}台）</h3><div class="dbg-row">`
      + `<button data-act="pause">${botPaused ? '再開する' : '一時停止'}</button>`
      + `<button data-act="speed">速さ ×${botSpeed === 1 ? '1 → ×10' : '10 → ×1'}</button></div></section>`
      + `<section><h3>アイテム（${['未出現', '盤面で待機中', '入手済み'][item.s]}）</h3><div class="dbg-row">`
      + `<button data-act="itemGet">入手済みにする</button><button data-act="itemReset">未出現に戻す</button></div></section>`
      + `<section><h3>セーブ</h3><div class="dbg-row"><button data-act="copySave">コピー</button><button data-act="pasteSave">貼り付け</button></div></section>`
      + `<section><h3>状態</h3><div class="dbg-info">1A ${blocks.size}　大タイル ${coarseCount}（次に開く大きさ ${coarseLevel() > 1 ? '1' + letter(coarseLevel()) : '1A'}）<br>`
      + `完成区画 ${doneCounts.join(' ') || 'なし'}　拡大率 ${cam.z < 0.01 ? cam.z.toExponential(1) : cam.z.toFixed(3)}</div></section>`;
  }
  function setDebug(open) {
    debugOpen = open;
    debugEl.hidden = !open;
    debugBtn.setAttribute('aria-expanded', open ? 'true' : 'false');
    clearInterval(debugTimer);
    // Refresh the numbers now and then — but not while the pointer is on the panel (a click could miss its button).
    if (open) { renderDebug(); debugTimer = setInterval(() => { if (!debugEl.matches(':hover')) renderDebug(); }, 700); }
  }
  debugBtn.addEventListener('click', () => { setDebug(!debugOpen); debugBtn.blur(); });
  debugEl.addEventListener('click', (e) => {
    const btn = e.target.closest('button');
    if (!btn) return;
    btn.blur();
    if (btn.dataset.grant) {
      const [t, n] = btn.dataset.grant.split(':');
      debugBonus[Number(t)] += BigInt(n);
    } else {
      const a = DEBUG_ACTIONS[btn.dataset.act];
      if (!a || (a.ask && !confirm(a.ask + '（デバッグ用）'))) return;
      a.run();
    }
    requestUi();
    saveNow();
    renderDebug();
  });

  soundBtn.addEventListener('click', () => {
    soundOn = !soundOn;
    updateSoundBtn();
    if (soundOn) { sound.unlock(); sound.flip(); }
    saveNow();
  });
  document.getElementById('btn-zoom-in').addEventListener('click', () => zoomBy(1.4));
  document.getElementById('btn-zoom-out').addEventListener('click', () => zoomBy(1 / 1.4));
  document.getElementById('btn-home').addEventListener('click', goHome);
  // Buttons shouldn't keep focus (Space must stay free for panning).
  for (const btn of document.querySelectorAll('.btn, .upgrade, .reset')) {
    btn.addEventListener('pointerup', () => btn.blur());
    btn.addEventListener('pointerdown', () => sound.unlock());
  }

  // First-time hint
  let hintFlips = 0, hintTimer = 0, hintShown = false;
  function showHint() {
    if (hintSeen) return;
    hintEl.hidden = false;
    hintShown = true;
    requestAnimationFrame(() => hintEl.classList.add('show'));
    hintTimer = setTimeout(hideHint, 12000);
  }
  function hideHint() {
    if (!hintShown) return;
    hintShown = false;
    clearTimeout(hintTimer);
    hintEl.classList.remove('show');
    setTimeout(() => { hintEl.hidden = true; }, 1300);
    hintSeen = true;
    saveNow();
  }

  // ---------- Machines: movement ----------
  // Each walks to the nearest white tile — in its current 1A first, otherwise the nearest unfinished
  // open 1A (any tier works anywhere) — and turns up to its capacity there.
  const BOT_REST_MS = 380;
  let botPaused = false, botSpeed = 1; // debug panel
  const tileCenter = (b, i) => ({ x: b.x * PITCH + (i % N) * CELL + TILE / 2, y: b.y * PITCH + ((i / N) | 0) * CELL + TILE / 2 });

  const bigCenter = (L, ux, uy) => { const t = bigTileRect(L, ux, uy); return { x: t.x + t.w / 2, y: t.y + t.w / 2 }; };
  function botTarget(bot) {
    if (bot.big && coarse[bot.big[0]].has(key(bot.big[1], bot.big[2])) && !unitComplete(...bot.big)) return { big: bot.big };
    const cur = bot.b && blocks.get(key(bot.b.x, bot.b.y)) === bot.b ? bot.b : null;
    if (cur && !cur.done && blockPlayable(cur.x, cur.y)) {
      for (let k = 0; k < 64; k++) { const j = (bot.i + k) % 64; if (!isBlack(cur, j)) return { b: cur, i: j }; }
    }
    // Nearest big tile too (another machine on it is fine: big tiles are big).
    let big = null, bigD = Infinity;
    for (let L = 2; L <= MAX_LEVEL; L++) for (const k of coarse[L].keys()) {
      const [ux, uy] = k.split(',').map(Number);
      const c = bigCenter(L, ux, uy), d = Math.hypot(c.x - bot.x, c.y - bot.y);
      if (d < bigD) { bigD = d; big = [L, ux, uy]; }
    }
    // Nearest unfinished 1A, preferring one no other machine is working on.
    const taken = new Set(bots.filter((o) => o !== bot && o.b).map((o) => o.b));
    let best = null, bd = Infinity, bestTaken = null, bdTaken = Infinity;
    for (const b of blocks.values()) {
      if (b.done || !blockPlayable(b.x, b.y)) continue;
      const d = Math.hypot(b.x * PITCH + BLOCK / 2 - bot.x, b.y * PITCH + BLOCK / 2 - bot.y);
      if (taken.has(b)) { if (d < bdTaken) { bdTaken = d; bestTaken = b; } }
      else if (d < bd) { bd = d; best = b; }
    }
    best = best || bestTaken;
    const bestD = !best ? Infinity : best === bestTaken ? bdTaken : bd;
    if (big && bigD < bestD) return { big };
    if (!best) return null;
    // Enter the block at the white tile closest to the machine.
    let bi = -1, bdist = Infinity;
    for (let i = 0; i < 64; i++) {
      if (isBlack(best, i)) continue;
      const c = tileCenter(best, i), d = Math.hypot(c.x - bot.x, c.y - bot.y);
      if (d < bdist) { bdist = d; bi = i; }
    }
    return bi < 0 ? null : { b: best, i: bi };
  }

  function botStep(bot) {
    bot.timer = 0;
    if (botPaused) { bot.timer = setTimeout(() => botStep(bot), 400); return; }
    const tgt = botTarget(bot);
    if (!tgt) { bot.timer = setTimeout(() => botStep(bot), 1000); return; } // nothing to do yet
    const to = tgt.big ? bigCenter(...tgt.big) : tileCenter(tgt.b, tgt.i);
    const dist = Math.hypot(to.x - bot.x, to.y - bot.y);
    const dur = Math.min(1400, 120 + dist * 2.5) / botSpeed;
    bot.glide = { fx: bot.x, fy: bot.y, tx: to.x, ty: to.y, t0: performance.now(), dur };
    bot.big = tgt.big || null;
    if (!tgt.big) { bot.b = tgt.b; bot.i = tgt.i; }
    requestRender();
    bot.timer = setTimeout(() => {
      bot.x = to.x; bot.y = to.y; bot.glide = null;
      if (tgt.big) {
        const [L, ux, uy] = tgt.big;
        if (coarse[L].has(key(ux, uy)) && !unitComplete(L, ux, uy)) {
          const job = workUnit(capOf(botExp(bot)), L, ux, uy, newJob(performance.now(), 0.45));
          if (job.flipped || job.completed) bot.pulse = job.now;
        }
        requestRender();
        bot.timer = setTimeout(() => botStep(bot), BOT_REST_MS / botSpeed);
        return;
      }
      const b = blocks.get(key(tgt.b.x, tgt.b.y));
      if (b && !b.done && blockPlayable(b.x, b.y)) {
        const job = work(capOf(botExp(bot)), b, tgt.i, newJob(performance.now(), 0.45));
        if (job.flipped || job.completed) bot.pulse = job.now;
      }
      requestRender();
      bot.timer = setTimeout(() => botStep(bot), BOT_REST_MS / botSpeed);
    }, dur);
  }

  function addBot(spec, fresh) {
    const bot = { id: nextBotId++, tier: spec.tier, stage: spec.stage, legacy: !!spec.legacy,
      x: spec.x || 0, y: spec.y || 0, b: null, i: 0, glide: null, pulse: 0, timer: 0 };
    if (fresh || (!spec.x && !spec.y)) {
      // Appear in the middle of the view, nudged aside from earlier ones.
      const k = bots.length;
      bot.x = cam.x + (k % 3 - 1) * CELL; bot.y = cam.y + (Math.floor(k / 3) % 3 - 1) * CELL;
    }
    bots.push(bot);
    // Stagger start so several machines don't tick in lockstep.
    bot.timer = setTimeout(() => botStep(bot), 60 + (bots.length % 8) * 110);
    return bot;
  }

  // ---------- Input ----------
  let spaceDown = false;
  let mode = null; // 'press' | 'pan' | 'pinch' | 'void'
  const pointers = new Map();
  let last = null;        // last screen position for pan
  let pinch = null;
  let pressStart = null;  // pending click / drag start
  const DRAG_START_PX = 5;
  const LONG_PRESS_MS = 400;
  // With the item, dragging on tiles flips them like clicks — but at most one click per PAINT_GAP_MS,
  // and never twice on the same tile in a row, so a fast drag can't set off a flood.
  const PAINT_GAP_MS = 70;

  function setPanCursor() {
    document.body.classList.toggle('can-pan', spaceDown && mode !== 'pan');
    document.body.classList.toggle('panning', mode === 'pan');
  }

  /** One click: the tile under the pointer plus whatever the manual stage adds. */
  function clickAt(sx, sy, now) {
    const w = screenToWorld(sx, sy);
    const big = bigTileAt(w.x, w.y);
    if (big) { workUnit(capOf(manualExp(manualLevel)), big[0], big[1], big[2], newJob(now, 1)); return; }
    const hit = tileAt(w.x, w.y);
    if (!hit || hit.b.done || isBlack(hit.b, hit.i) || !blockPlayable(hit.b.x, hit.b.y)) return;
    const job = work(capOf(manualExp(manualLevel)), hit.b, hit.i, newJob(now, 1));
    if (hintShown && (hintFlips += job.flipped) >= 8) hideHint();
  }

  // Drag (with the item): the tile under the pointer gets a manual click, paced by PAINT_GAP_MS.
  let paintTick = 0, paintLastId = null, paintLastAt = -Infinity;
  function startPaint() {
    mode = 'paint';
    paintLastId = null; paintLastAt = -Infinity;
    document.body.classList.add('painting');
    paintTry(pressStart.x, pressStart.y);   // the tile where the drag began
    paintTry(last.x, last.y);
    clearInterval(paintTick);
    // While the pointer rests on a new tile, it still gets its click once the gap has passed.
    paintTick = setInterval(() => { if (mode === 'paint' && last) paintTry(last.x, last.y); }, PAINT_GAP_MS / 2);
  }
  function stopPaint() {
    clearInterval(paintTick); paintTick = 0;
    document.body.classList.remove('painting');
  }
  function paintTry(sx, sy) {
    const now = performance.now();
    if (now - paintLastAt < PAINT_GAP_MS) return;
    const w = screenToWorld(sx, sy);
    const big = bigTileAt(w.x, w.y);
    if (big) {
      const bid = 'big' + big.join(',');
      if (bid === paintLastId) return;
      paintLastId = bid; paintLastAt = now;
      workUnit(capOf(manualExp(manualLevel)), big[0], big[1], big[2], newJob(now, 0.8));
      return;
    }
    const hit = tileAt(w.x, w.y);
    if (!hit) return;
    const id = hit.b.x + ',' + hit.b.y + ',' + hit.i;
    if (id === paintLastId || hit.b.done || isBlack(hit.b, hit.i) || !blockPlayable(hit.b.x, hit.b.y)) return;
    paintLastId = id; paintLastAt = now;
    work(capOf(manualExp(manualLevel)), hit.b, hit.i, newJob(now, 0.8));
  }

  canvas.addEventListener('pointerdown', (e) => {
    sound.unlock();
    if (e.pointerType === 'touch') document.body.classList.add('touch');
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    try { canvas.setPointerCapture(e.pointerId); } catch (_) {}

    if (e.pointerType === 'touch' && pointers.size >= 2) {
      // Two fingers → pinch/pan.
      const [p1, p2] = [...pointers.values()];
      const mx = (p1.x + p2.x) / 2, my = (p1.y + p2.y) / 2;
      const w = screenToWorld(mx, my);
      pinch = { d0: Math.hypot(p1.x - p2.x, p1.y - p2.y) || 1, z0: cam.z, wx: w.x, wy: w.y };
      stopPaint();
      mode = 'pinch';
      tween = null;
      return;
    }
    if (pointers.size > 1) return;

    tween = null;
    last = { x: e.clientX, y: e.clientY };
    if ((e.button === 0 && spaceDown) || e.button === 1 || e.button === 2) {
      mode = 'pan';
      setPanCursor();
      e.preventDefault();
    } else if (e.button === 0) {
      const w = screenToWorld(e.clientX, e.clientY);
      if (isTileArea(w.x, w.y)) {
        // On a tile: a quick release flips it. A long press does nothing.
        // Dragging does nothing either — unless the item is owned, then the drag flips tiles.
        mode = 'press';
        pressStart = { x: e.clientX, y: e.clientY, t: performance.now() };
      } else {
        // On the background: drag moves the board.
        mode = 'pan';
        setPanCursor();
      }
    }
  });

  canvas.addEventListener('pointermove', (e) => {
    const p = pointers.get(e.pointerId);
    if (!p) {
      // Hover: show a grab cursor over the background.
      if (e.pointerType === 'mouse' && pointers.size === 0) {
        const w = screenToWorld(e.clientX, e.clientY);
        document.body.classList.toggle('over-bg', !isTileArea(w.x, w.y));
      }
      return;
    }
    p.x = e.clientX; p.y = e.clientY;

    if (mode === 'pinch') {
      if (pointers.size < 2) return;
      const [p1, p2] = [...pointers.values()];
      const mx = (p1.x + p2.x) / 2, my = (p1.y + p2.y) / 2;
      const d = Math.hypot(p1.x - p2.x, p1.y - p2.y) || 1;
      cam.z = clamp(pinch.z0 * d / pinch.d0, minZoom(), MAX_ZOOM);
      cam.x = pinch.wx - (mx - W / 2) / cam.z;
      cam.y = pinch.wy - (my - H / 2) / cam.z;
      settleAtMinZoom();
      requestRender();
      return;
    }
    if (!last) return;
    if (mode === 'paint') {
      last = { x: e.clientX, y: e.clientY };
      paintTry(last.x, last.y);
      return;
    }
    if (mode === 'press') {
      if (Math.hypot(e.clientX - pressStart.x, e.clientY - pressStart.y) < DRAG_START_PX) return;
      last = { x: e.clientX, y: e.clientY };
      if (item.s === 2) { startPaint(); pressStart = null; return; }
      // Without the item, dragging on a tile does nothing: no flip, no pan.
      mode = 'void';
      pressStart = null;
    }
    if (mode === 'pan') {
      cam.x -= (e.clientX - last.x) / cam.z;
      cam.y -= (e.clientY - last.y) / cam.z;
      last = { x: e.clientX, y: e.clientY };
      requestRender();
    }
  });

  function endPointer(e) {
    if (!pointers.has(e.pointerId)) return;
    pointers.delete(e.pointerId);
    if (mode === 'paint') { stopPaint(); scheduleSave(); }
    if (mode === 'pinch') {
      if (pointers.size === 0) { mode = null; pinch = null; last = null; scheduleSave(); }
      // with one finger left, stay idle (don't flip by accident)
      return;
    }
    if (pointers.size === 0) {
      if (mode === 'pan') scheduleSave();
      if (mode === 'press' && e.type === 'pointerup' && performance.now() - pressStart.t < LONG_PRESS_MS) {
        clickAt(pressStart.x, pressStart.y, performance.now());
      }
      pressStart = null;
      mode = null;
      last = null;
      setPanCursor();
    }
  }
  canvas.addEventListener('pointerup', endPointer);
  canvas.addEventListener('pointercancel', endPointer);
  canvas.addEventListener('lostpointercapture', endPointer);
  canvas.addEventListener('contextmenu', (e) => e.preventDefault());

  canvas.addEventListener('wheel', (e) => {
    e.preventDefault();
    let dy = e.deltaY;
    if (e.deltaMode === 1) dy *= 16; else if (e.deltaMode === 2) dy *= H;
    const factor = Math.exp(-clamp(dy, -200, 200) * (e.ctrlKey ? 0.01 : 0.0018));
    zoomBy(factor, e.clientX, e.clientY, false);
  }, { passive: false });

  window.addEventListener('keydown', (e) => {
    if (e.code === 'Space') {
      e.preventDefault();
      if (!spaceDown) { spaceDown = true; setPanCursor(); }
    } else if (e.key === '+' || e.key === '=') zoomBy(1.4);
    else if (e.key === '-' || e.key === '_') zoomBy(1 / 1.4);
  });
  window.addEventListener('keyup', (e) => {
    if (e.code === 'Space') { e.preventDefault(); spaceDown = false; setPanCursor(); }
  });
  window.addEventListener('blur', () => { spaceDown = false; setPanCursor(); });
  window.addEventListener('resize', resize);
  window.addEventListener('pagehide', saveNow);
  document.addEventListener('visibilitychange', () => { if (document.hidden) saveNow(); });

  // ---------- Sound ----------
  const sound = (() => {
    let ac = null, master = null, wet = null;
    let lastFlip = 0, energy = 0, energyAt = 0;
    let chimeE = 0, chimeAt = 0, chimeNote = 0; // how busy the completion chime has been lately

    function build() {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return;
      ac = new AC();
      const comp = ac.createDynamicsCompressor();
      comp.threshold.value = -26; comp.knee.value = 18; comp.ratio.value = 5;
      comp.attack.value = 0.003; comp.release.value = 0.25;
      master = ac.createGain();
      master.gain.value = 0.9;
      master.connect(comp).connect(ac.destination);
      // Soft echo tail used for the completion chime.
      const delay = ac.createDelay(1.5);
      delay.delayTime.value = 0.31;
      const fb = ac.createGain(); fb.gain.value = 0.38;
      const lp = ac.createBiquadFilter(); lp.type = 'lowpass'; lp.frequency.value = 1500;
      wet = ac.createGain(); wet.gain.value = 0.5;
      wet.connect(delay); delay.connect(lp); lp.connect(fb); fb.connect(delay); lp.connect(master);
    }

    function unlock() {
      if (!soundOn) return;
      if (!ac) build();
      if (ac && ac.state === 'suspended') ac.resume();
    }

    const ready = () => soundOn && ac && ac.state === 'running';

    function flip(vol = 1) {
      if (!ready()) return;
      const t = ac.currentTime;
      if (t - lastFlip < 0.028) return; // collapse bursts
      // decaying density → quieter when many tiles flip in a row
      energy = energy * Math.exp(-(t - energyAt) / 0.35) + 1;
      energyAt = t;
      lastFlip = t;
      const g = 0.11 * vol / (1 + (energy - 1) * 0.35);
      const f = 640 * Math.pow(2, (Math.random() - 0.5) * 0.16);

      const osc = ac.createOscillator();
      osc.type = 'sine';
      osc.frequency.setValueAtTime(f, t);
      osc.frequency.exponentialRampToValueAtTime(f * 0.72, t + 0.07);
      const lp = ac.createBiquadFilter(); lp.type = 'lowpass'; lp.frequency.value = 1700;
      const env = ac.createGain();
      env.gain.setValueAtTime(0.0001, t);
      env.gain.exponentialRampToValueAtTime(g, t + 0.005);
      env.gain.exponentialRampToValueAtTime(0.0001, t + 0.13);
      osc.connect(lp).connect(env).connect(master);
      osc.start(t);
      osc.stop(t + 0.15);
    }

    const CHIME = [523.25, 783.99, 1046.5, 1318.51];

    function complete() {
      if (!ready()) return;
      const now = ac.currentTime;
      if (now - chimeAt < 0.12) return; // several completions in the same instant → one sound
      chimeE = chimeE * Math.exp(-(now - chimeAt) / 2.0) + 1;
      chimeAt = now;
      if (chimeE > 1.35) {
        // Completions coming in quick succession: just one soft note each (walking up the chord),
        // quieter the busier it gets, with no echo — so it never piles up.
        const f = CHIME[chimeNote++ % CHIME.length];
        const t = now + 0.06;
        const g = 0.032 / Math.sqrt(chimeE);
        const osc = ac.createOscillator();
        osc.type = 'sine';
        osc.frequency.value = f;
        const env = ac.createGain();
        env.gain.setValueAtTime(0.0001, t);
        env.gain.exponentialRampToValueAtTime(g, t + 0.02);
        env.gain.exponentialRampToValueAtTime(0.0001, t + 1.3);
        osc.connect(env).connect(master);
        osc.start(t);
        osc.stop(t + 1.4);
        return;
      }
      chimeNote = 0;
      const t0 = now + 0.08;
      CHIME.forEach((f, k) => {
        const t = t0 + k * 0.11;
        const osc = ac.createOscillator();
        osc.type = 'sine';
        osc.frequency.value = f;
        const env = ac.createGain();
        const g = 0.05 - k * 0.007;
        env.gain.setValueAtTime(0.0001, t);
        env.gain.exponentialRampToValueAtTime(g, t + 0.03);
        env.gain.exponentialRampToValueAtTime(0.0001, t + 2.6);
        osc.connect(env);
        env.connect(master);
        env.connect(wet);
        osc.start(t);
        osc.stop(t + 2.7);
      });
    }

    /** A multi-tile flip: a few soft ticks spread across the wave instead of one per tile. */
    function wave(durMs, ticks) {
      for (let k = 1; k <= ticks; k++) {
        setTimeout(() => flip(0.6 * Math.pow(0.72, k - 1)), (durMs * k) / ticks);
      }
    }

    /** Picking up the item: a soft rising run with a little echo. */
    function itemGet() {
      if (!ready()) return;
      const t0 = ac.currentTime + 0.05;
      [659.25, 783.99, 987.77, 1174.66, 1567.98].forEach((f, k) => {
        const t = t0 + k * 0.09;
        const osc = ac.createOscillator();
        osc.type = 'sine';
        osc.frequency.value = f;
        const env = ac.createGain();
        env.gain.setValueAtTime(0.0001, t);
        env.gain.exponentialRampToValueAtTime(0.038 - k * 0.004, t + 0.02);
        env.gain.exponentialRampToValueAtTime(0.0001, t + 1.9);
        osc.connect(env); env.connect(master); env.connect(wet);
        osc.start(t); osc.stop(t + 2);
      });
    }

    return { unlock, flip, complete, wave, item: itemGet };
  })();

  // ---------- Boot ----------
  load();
  resize();
  for (const spec of savedBots) addBot(spec, false);
  updatePoints();
  updateSoundBtn();
  applyTheme();
  if (!hintSeen) setTimeout(showHint, 700);
  if (matchMedia('(pointer: coarse)').matches) document.body.classList.add('touch');

  // Small hook for manual testing in devtools. debug.grant() changes points in memory only (gone on reload).
  window.__8x8 = {
    blocks, cam, bots, cleared, spent, doneSet,
    avail, buy, capLabel, botExp, manualExp, manualPrice, upgradeCost, fmt,
    get manualLevel() { return manualLevel; },
    get points() { return avail(1); },
    minZoom, worldExtent, isOpenArea, unitComplete,
    item, itemCenter, coarse, coarseLevel, black, bigAnim, bigGroup,
    debug: { grant(L, n) { debugBonus[L] += BigInt(n); requestUi(); } },
    tileCenterScreen(bx, by, i) {
      const wx = bx * PITCH + (i % N) * CELL + TILE / 2, wy = by * PITCH + ((i / N) | 0) * CELL + TILE / 2;
      return { x: (wx - cam.x) * cam.z + W / 2, y: (wy - cam.y) * cam.z + H / 2 };
    },
  };
})();
