/* ════════════════ TOC DE PAGE À DROITE (façon Dask) ════════════════
 * Sommaire intra-page généré depuis les h2/h3 du contenu, colonne fixe à
 * droite, scrollspy (section active en vert), caché sous 1200px. Utilise
 * l'espace droit + navigation dans les longues pages. */
(function () {
  'use strict';
  if (/(^|\/)print\.html$/.test(window.location.pathname)) return;
  // Thèmes CALIBRÉS uniquement (light sauge + coal dark). Un thème non stylé par
  // notre charte (ayu/navy/rust) persisté en localStorage → le ramener sur light.
  try {
    var _t = localStorage.getItem('mdbook-theme');
    if (_t === 'ayu' || _t === 'navy' || _t === 'rust') {
      localStorage.setItem('mdbook-theme', 'light');
      var _h = document.documentElement;
      _h.classList.remove('ayu', 'navy', 'rust', 'coal');
      _h.classList.add('light');
    }
  } catch (e) {}
  var reducedMotion = window.matchMedia &&
                      window.matchMedia('(prefers-reduced-motion: reduce)');
  function build() {
    var content = document.querySelector('.content main') || document.querySelector('.content');
    if (!content) return;
    var heads = content.querySelectorAll('h2[id], h3[id]');
    if (heads.length < 2) return;   // pas de TOC pour une page courte

    var nav = document.createElement('nav');
    nav.className = 'page-toc';
    nav.setAttribute('aria-label', 'On this page');
    var title = document.createElement('p');
    title.className = 'page-toc-title';
    title.textContent = 'On this page';
    nav.appendChild(title);

    var links = [];
    heads.forEach(function (h) {
      var a = document.createElement('a');
      a.href = '#' + h.id;
      a.textContent = h.textContent.replace(/[¶#]\s*$/, '').trim();
      a.className = 'page-toc-link lvl-' + h.tagName.toLowerCase();
      a.addEventListener('click', function (e) {
        e.preventDefault();
        var t = document.getElementById(h.id);
        if (t) { t.scrollIntoView({ behavior: reducedMotion && reducedMotion.matches ? 'auto' : 'smooth', block: 'start' });
                 history.replaceState(null, '', '#' + h.id); }
      });
      nav.appendChild(a);
      links.push({ id: h.id, el: a, head: h });
    });

    var page = document.querySelector('.page') || content.parentNode;
    page.appendChild(nav);

    // scrollspy — surligne la section la plus proche du haut
    var ticking = false;
    var scroller = document.querySelector('.content') || window;
    function atBottom() {
      if (scroller !== window && scroller.scrollHeight > scroller.clientHeight)
        return scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2;
      var root = document.documentElement;
      return window.scrollY + window.innerHeight >= root.scrollHeight - 2;
    }
    function spy() {
      ticking = false;
      var best = null, bestTop = -Infinity;
      var mark = 120;
      if (atBottom()) {
        best = links[links.length - 1];
      } else {
        for (var i = 0; i < links.length; i++) {
          var top = links[i].head.getBoundingClientRect().top;
          if (top <= mark && top > bestTop) { bestTop = top; best = links[i]; }
        }
      }
      if (!best) best = links[0];
      for (var j = 0; j < links.length; j++) {
        var active = links[j] === best;
        links[j].el.classList.toggle('active', active);
        if (active) links[j].el.setAttribute('aria-current', 'location');
        else links[j].el.removeAttribute('aria-current');
      }
    }
    (scroller === window ? window : scroller).addEventListener('scroll', function () {
      if (!ticking) { ticking = true; requestAnimationFrame(spy); }
    }, { passive: true });
    window.addEventListener('scroll', function () {
      if (!ticking) { ticking = true; requestAnimationFrame(spy); }
    }, { passive: true });
    spy();
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', build);
  else build();

  // ── le titre/logo « ∴ Noogram » ramène au site principal ──────────────
  function linkLogo() {
    var mt = document.querySelector('.menu-title');
    if (!mt || mt.querySelector('a')) return;
    var a = document.createElement('a');
    a.href = 'https://noogram.org';
    a.setAttribute('aria-label', 'noogram.org — home');
    a.style.color = 'inherit';
    a.style.textDecoration = 'none';
    a.style.display = 'inline-flex';
    a.style.alignItems = 'baseline';
    while (mt.firstChild) a.appendChild(mt.firstChild);
    mt.appendChild(a);
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', linkLogo);
  else linkLogo();

  // Place mdBook's native theme menu beside the other page actions and give
  // its paint-brush trigger an explicit light/dark label.
  function exposeThemeToggle() {
    var toggle = document.getElementById('theme-toggle');
    var list = document.getElementById('theme-list');
    var actions = document.querySelector('.right-buttons');
    if (!toggle || !list || !actions || toggle.closest('.theme-control')) return;

    var control = document.createElement('div');
    control.className = 'theme-control';
    toggle.title = 'Choose light or dark theme';
    toggle.setAttribute('aria-label', 'Choose light or dark theme');
    control.appendChild(toggle);
    control.appendChild(list);
    actions.insertBefore(control, actions.firstChild);
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', exposeThemeToggle);
  else exposeThemeToggle();

  // Give narrow viewports readable, labelled table rows without duplicating
  // headings in Markdown. CSS uses these labels to stack each row as a card.
  function labelResponsiveTables() {
    document.querySelectorAll('.content table').forEach(function (table) {
      var headings = Array.prototype.map.call(
        table.querySelectorAll('thead th'),
        function (heading) { return heading.textContent.trim(); }
      );
      if (!headings.length) return;
      table.classList.add('responsive-table');
      table.querySelectorAll('tbody tr').forEach(function (row) {
        Array.prototype.forEach.call(row.children, function (cell, index) {
          cell.setAttribute('data-label', headings[index] || 'Value');
        });
      });
    });
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', labelResponsiveTables);
  else labelResponsiveTables();

  // ── Highlight the current chapter in the LEFT sidebar ─────────────────
  // mdBook serves one shared toc.js sidebar for every page and bakes no
  // per-page active marker, so the charte's green `a.active` /
  // `a[aria-current="page"]` rules never match. Flag the link whose resolved
  // path equals the current page ourselves — the same job the right page-TOC
  // scrollspy already does for on-page sections.
  // Normalize so clean URLs match .html hrefs: the host (Cloudflare Pages)
  // serves `/reference/execution` while the sidebar link resolves to
  // `.../execution.html`. Strip `/index.html`, the `.html` suffix, and any
  // trailing slash so both sides compare equal.
  function normPath(p) {
    return p.replace(/\/index\.html$/, '/').replace(/\.html$/, '')
            .replace(/\/+$/, '') || '/';
  }
  function markSidebarActive() {
    var here = normPath(window.location.pathname);
    var links = document.querySelectorAll('#sidebar .chapter a[href]');
    if (!links.length) return false;
    var matched = false;
    links.forEach(function (a) {
      var url;
      try { url = new URL(a.getAttribute('href'), window.location.href); }
      catch (e) { return; }
      var on = normPath(url.pathname) === here;
      a.classList.toggle('active', on);
      if (on) { a.setAttribute('aria-current', 'page'); matched = true; }
      else a.removeAttribute('aria-current');
    });
    return matched;
  }
  // The sidebar is a custom element that may hydrate a tick after load; retry
  // briefly until its links exist, then stop.
  function armSidebarActive() {
    if (markSidebarActive()) return;
    var tries = 0;
    var iv = setInterval(function () {
      if (markSidebarActive() || ++tries > 20) clearInterval(iv);
    }, 50);
  }
  if (document.readyState === 'loading')
    document.addEventListener('DOMContentLoaded', armSidebarActive);
  else armSidebarActive();
})();
