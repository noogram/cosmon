/* ════════════════ CHAMP CONWAY — papier peint figé des docs ════════════════
 * Le Jeu de la Vie (B3/S23) est semé, avancé de quelques générations puis
 * peint une seule fois. Aucun moteur d'animation ne reste actif. */
(function () {
  'use strict';
  if (/(^|\/)print\.html$/.test(window.location.pathname)) return;
  // canvas auto-créé (mdBook n'a pas de #field dans son template)
  var c = document.getElementById('noogram-field');
  if (!c) {
    c = document.createElement('canvas');
    c.id = 'noogram-field';
    c.setAttribute('aria-hidden', 'true');
    document.body.insertBefore(c, document.body.firstChild);
  }
  if (!c.getContext) return;
  var ctx = c.getContext('2d');
  if (!ctx) { c.remove(); return; }

  function isLight() { return document.documentElement.classList.contains('light'); }
  function rgb() { return isLight() ? '23,109,43' : '63,185,80'; }
  var ALPHA = 0.05;
  var CELL = 18;
  var cols = 0, rows = 0, dpr = 1;
  var cur, nxt;

  var GLIDER  = [[1,0],[2,1],[0,2],[1,2],[2,2]];
  var BLINKER = [[0,0],[1,0],[2,0]];
  var BLOCK   = [[0,0],[1,0],[0,1],[1,1]];
  var LWSS    = [[1,0],[4,0],[0,1],[0,2],[4,2],[0,3],[1,3],[2,3],[3,3]];

  var rng = 0x2545f491 | 0;
  function rand() {
    rng ^= rng << 13; rng |= 0;
    rng ^= rng >>> 17;
    rng ^= rng << 5;  rng |= 0;
    return (rng >>> 0) / 4294967296;
  }
  function randInt(n) { return (rand() * n) | 0; }

  function rot(cells, k) {
    var out = cells.map(function (p) { return [p[0], p[1]]; });
    for (var r = 0; r < (k & 3); r++) {
      var maxY = 0;
      for (var i = 0; i < out.length; i++) if (out[i][1] > maxY) maxY = out[i][1];
      out = out.map(function (p) { return [maxY - p[1], p[0]]; });
    }
    return out;
  }
  function stamp(cells, ox, oy) {
    for (var i = 0; i < cells.length; i++) {
      var x = ((ox + cells[i][0]) % cols + cols) % cols;
      var y = ((oy + cells[i][1]) % rows + rows) % rows;
      cur[y * cols + x] = 1;
    }
  }
  function dims() {
    cols = Math.max(8, Math.ceil(window.innerWidth / CELL));
    rows = Math.max(8, Math.ceil(window.innerHeight / CELL));
  }
  function seed() {
    cur = new Uint8Array(cols * rows);
    nxt = new Uint8Array(cols * rows);
    var area = cols * rows;
    var nGlider = Math.max(5, Math.round(area / 900));
    var nLwss   = Math.max(1, Math.round(area / 4200));
    var nBlink  = Math.max(2, Math.round(area / 2600));
    var i;
    for (i = 0; i < nGlider; i++) stamp(rot(GLIDER, randInt(4)), randInt(cols), randInt(rows));
    for (i = 0; i < nLwss;   i++) stamp(rot(LWSS,   randInt(4)), randInt(cols), randInt(rows));
    for (i = 0; i < nBlink;  i++) stamp(rot(BLINKER, randInt(2)), randInt(cols), randInt(rows));
    for (i = 0; i < Math.max(1, nBlink >> 1); i++) stamp(BLOCK, randInt(cols), randInt(rows));
  }
  function generation() {
    for (var y = 0; y < rows; y++) {
      var ym = (y - 1 + rows) % rows, yp = (y + 1) % rows;
      for (var x = 0; x < cols; x++) {
        var xm = (x - 1 + cols) % cols, xp = (x + 1) % cols;
        var n = cur[ym * cols + xm] + cur[ym * cols + x] + cur[ym * cols + xp]
              + cur[y  * cols + xm]                      + cur[y  * cols + xp]
              + cur[yp * cols + xm] + cur[yp * cols + x] + cur[yp * cols + xp];
        var alive = cur[y * cols + x];
        nxt[y * cols + x] = (n === 3 || (alive && n === 2)) ? 1 : 0;
      }
    }
    var t = cur; cur = nxt; nxt = t;
    var pop = 0, k;
    for (k = 0; k < cur.length; k++) if (cur[k]) pop++;
    if (pop < (cols * rows) * 0.010) {
      if (randInt(2)) stamp(rot(GLIDER, randInt(4)), randInt(cols), randInt(rows));
      else            stamp(rot(LWSS,   randInt(4)), randInt(cols), randInt(rows));
    }
  }
  function size() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    c.style.width = window.innerWidth + 'px';
    c.style.height = window.innerHeight + 'px';
    c.width  = Math.round(window.innerWidth * dpr);
    c.height = Math.round(window.innerHeight * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  function draw() {
    ctx.clearRect(0, 0, window.innerWidth, window.innerHeight);
    ctx.fillStyle = 'rgba(' + rgb() + ',' + ALPHA + ')';
    var pad = CELL * 0.30, sz = CELL - pad * 2;
    for (var y = 0; y < rows; y++) {
      var row = y * cols;
      for (var x = 0; x < cols; x++) {
        if (cur[row + x]) ctx.fillRect(x * CELL + pad, y * CELL + pad, sz, sz);
      }
    }
  }
  function freeze() { for (var s = 0; s < 24; s++) generation(); draw(); }
  function init() { size(); dims(); seed(); freeze(); }
  init();

  window.addEventListener('resize', function () {
    size(); dims(); seed(); freeze();
  });
  // Re-teinte le papier peint une fois lors d'un changement de thème mdBook.
  var mo = new MutationObserver(draw);
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] });
})();
