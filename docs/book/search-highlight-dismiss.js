/* ════════════ SURLIGNAGE DE RECHERCHE — CONGÉDIABLE ════════════
 * mdBook surligne l'atterrissage d'un résultat de recherche à partir de
 * `?highlight=<phrase>`, en découpant la phrase par espaces et en marquant
 * chaque mot *partiellement* (mark.js, accuracy « partially »). Une phrase
 * un peu longue transforme donc la page en damier : « over » est surligné
 * dans « discover », « takeover », « HAND-OVER », « takeover.pub ».
 *
 * En amont, mdBook ne prévoit qu'une seule sortie : cliquer précisément SUR
 * un <mark>. Rater la cible, ou recharger, et le damier revient — le
 * paramètre reste dans l'URL, donc dans le lien que le lecteur copie.
 *
 * Ce script élargit la sortie sans forker le thème : le surlignage survit
 * assez longtemps pour montrer où l'on a atterri, puis disparaît au premier
 * geste de lecture (clic, Échap, défilement franc), et l'URL est nettoyée
 * pour qu'un rechargement ou un lien partagé n'ouvre pas sur le damier.
 *
 * Ce qui n'est PAS touché : la recherche elle-même (`?search=`), l'ancre
 * (`#cs-sessions`) et l'autonomie hors-ligne (aucune ressource externe,
 * aucune dépendance à mark.js — on désenveloppe nous-mêmes). */
(function () {
  'use strict';
  if (/(^|\/)print\.html$/.test(window.location.pathname)) return;

  var MARK_PARAM = 'highlight';
  // Délai de grâce : le lecteur doit VOIR où il a atterri avant qu'on efface.
  // En dessous, un navigateur qui saute à l'ancre après le chargement efface
  // le repère avant même qu'il soit à l'écran.
  var GRACE_MS = 1200;
  // Défilement « franc » : au-delà, le lecteur a quitté le point d'atterrissage
  // de son plein gré. Le saut d'ancre lui-même se produit avant l'armement et
  // sert de référence, il ne compte donc pas.
  var SCROLL_PX = 240;
  // Durée de la transition `background-color` de mark dans chrome.css.
  var FADE_MS = 300;
  // Seuls les <mark> posés par mark.js portent cet attribut. Un <mark> écrit à
  // la main dans une page est une intention d'auteur : on n'y touche jamais.
  var MARK_SELECTOR = 'mark[data-markjs]';

  /** Désenveloppe les <mark> posés par mark.js, sans passer par la librairie :
   * l'instance `Mark` de searcher.js n'est pas exposée, et redemander la
   * librairie ferait dépendre l'effacement d'un script tiers déjà chargé. */
  function unwrapMarks(root) {
    var marks = root.querySelectorAll(MARK_SELECTOR);
    for (var i = 0; i < marks.length; i++) {
      var m = marks[i];
      var parent = m.parentNode;
      if (!parent) continue;
      while (m.firstChild) parent.insertBefore(m.firstChild, m);
      parent.removeChild(m);
      // mark.js a découpé les nœuds texte ; les recoller évite de laisser une
      // page fragmentée derrière nous (sélection, Ctrl+F du navigateur).
      if (parent.normalize) parent.normalize();
    }
    return marks.length;
  }

  /** Retire `?highlight=` de l'URL affichée en gardant le reste : les autres
   * paramètres et surtout l'ancre, qui est le repère de lecture. */
  function stripHighlightFromUrl() {
    var search = window.location.search;
    if (search.indexOf(MARK_PARAM + '=') === -1) return;
    var kept = [];
    var pairs = search.replace(/^\?/, '').split('&');
    for (var i = 0; i < pairs.length; i++) {
      if (!pairs[i]) continue;
      var key = pairs[i].split('=')[0];
      if (key !== MARK_PARAM) kept.push(pairs[i]);
    }
    var url = window.location.pathname +
              (kept.length ? '?' + kept.join('&') : '') +
              window.location.hash;
    try {
      window.history.replaceState(window.history.state, '', url);
    } catch (e) { /* file:// sans historique manipulable : on garde l'URL */ }
  }

  function arm() {
    // Ne s'armer que sur un atterrissage de recherche. Sans cette garde, une
    // page qui contiendrait un <mark> pour une autre raison verrait ce script
    // l'effacer au premier clic.
    if (window.location.search.indexOf(MARK_PARAM + '=') === -1) return;
    var content = document.getElementById('content') || document.body;
    if (!content || !content.querySelector(MARK_SELECTOR)) return;

    // Le lien copié ne doit jamais ramener le damier — on nettoie tout de
    // suite, pendant que le surlignage, lui, reste visible.
    stripHighlightFromUrl();

    var baseline = window.pageYOffset || document.documentElement.scrollTop || 0;
    var armed = false;
    var fired = false;

    function dismiss() {
      if (fired) return;
      fired = true;
      disconnect();
      var marks = content.querySelectorAll(MARK_SELECTOR);
      for (var i = 0; i < marks.length; i++) marks[i].classList.add('fade-out');
      window.setTimeout(function () { unwrapMarks(content); }, FADE_MS);
    }

    function onKey(e) {
      if (armed && (e.key === 'Escape' || e.key === 'Esc' || e.keyCode === 27)) dismiss();
    }
    function onClick() { if (armed) dismiss(); }
    function onScroll() {
      if (!armed) return;
      var y = window.pageYOffset || document.documentElement.scrollTop || 0;
      if (Math.abs(y - baseline) > SCROLL_PX) dismiss();
    }
    function disconnect() {
      document.removeEventListener('keydown', onKey, true);
      document.removeEventListener('click', onClick, true);
      window.removeEventListener('scroll', onScroll);
    }

    document.addEventListener('keydown', onKey, true);
    document.addEventListener('click', onClick, true);
    window.addEventListener('scroll', onScroll, {passive: true});

    window.setTimeout(function () {
      armed = true;
      // Le saut d'ancre a eu lieu : c'est d'ICI que se mesure le défilement
      // volontaire, sinon l'atterrissage se congédierait lui-même.
      baseline = window.pageYOffset || document.documentElement.scrollTop || 0;
    }, GRACE_MS);
  }

  // searcher.js pose les <mark> à l'analyse du document, avant nous (il est
  // déclaré plus haut dans le <body> que les `additional-js`). Une frame de
  // marge couvre un ordre différent sans jamais bloquer le rendu.
  window.requestAnimationFrame
    ? window.requestAnimationFrame(arm)
    : window.setTimeout(arm, 0);

  // Retour arrière vers une entrée d'historique qui portait encore le
  // paramètre : mdBook re-surligne, on ré-arme.
  window.addEventListener('popstate', function () {
    window.setTimeout(arm, 0);
  });
})();
