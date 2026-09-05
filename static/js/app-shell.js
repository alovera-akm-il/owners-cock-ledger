// Shared page-shell helpers, loaded (like jquery.min.js) as a plain
// blocking <script> on every page — not deferred, because several pages'
// own inline scripts call getCookie/apiCall synchronously at parse time
// (e.g. a top-level `apiCall('GET', ...).done(...)` at the bottom of the
// script), not just from inside event handlers, so these have to exist
// before that inline script runs. The mobile-menu and logout wiring have
// no such ordering requirement, so they self-register on DOM-ready, same
// pattern as notifications.js/countdown.js.

function getCookie(name) {
  const match = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
  return match ? decodeURIComponent(match[1]) : null;
}

function apiCall(method, path, body) {
  return $.ajax({
    url: path, method: method, contentType: 'application/json',
    headers: { 'X-CSRF-Token': getCookie('ocl_csrf') || '' },
    data: body !== undefined ? JSON.stringify(body) : undefined,
  });
}

function errorMessage(xhr, fallback) {
  return (xhr.responseJSON && xhr.responseJSON.error && xhr.responseJSON.error.message) || fallback;
}

// Canonical play-session status labels (Duplication Ledger §04) —
// submissive_play_sessions.html and play_session_templates.html already
// agreed on this exact map; play_session_detail.html had drifted to
// capitalized labels, "Pending judgement" instead of "awaiting
// judgement", and a lighter idle-state gray. Each page keeps its own
// `const STATUS_LABEL = PLAY_SESSION_STATUS_LABELS;` alias rather than
// referencing this name directly, so a page can't accidentally redeclare
// the same top-level identifier this script already declared.
const PLAY_SESSION_STATUS_LABELS = {
  scheduled: ['scheduled', 'bg-slate-800 text-slate-400'],
  in_progress: ['in progress', 'bg-emerald-500/10 text-emerald-400'],
  pending_judgement: ['awaiting judgement', 'bg-amber-500/10 text-amber-400'],
  completed: ['completed', 'bg-slate-800 text-slate-400'],
  cancelled: ['cancelled', 'bg-slate-800 text-slate-500'],
};

// Canonical green/yellow/red check-in palette (Duplication Ledger §04) —
// one source of truth for which hue each color maps to, even though the
// three call sites render genuinely different components (a full status
// banner, a color-picker button, a small pill badge) and so keep their
// own Tailwind class shapes rather than being forced into one visual
// treatment. Same alias-not-direct-reference reasoning as the status
// labels above.
const CHECKIN_COLORS = {
  banner: {
    green: { bg: 'bg-emerald-600/20', border: 'border-emerald-600/40', text: 'text-emerald-400', label: 'GREEN — SAFE / OK' },
    yellow: { bg: 'bg-amber-600/20', border: 'border-amber-600/40', text: 'text-amber-400', label: 'YELLOW — NEAR LIMIT' },
    red: { bg: 'bg-red-600/20', border: 'border-red-600/40', text: 'text-red-400', label: 'RED — IMMEDIATE STOP' },
  },
  picker: {
    green: 'border-emerald-500 bg-emerald-500/10 text-emerald-400',
    yellow: 'border-amber-500 bg-amber-500/10 text-amber-400',
    red: 'border-red-500 bg-red-500/10 text-red-400',
  },
  badge: {
    green: 'text-emerald-400 bg-emerald-500/10',
    yellow: 'text-amber-400 bg-amber-500/10',
    red: 'text-red-400 bg-red-500/10',
  },
};

// Every modal in the app is a `#{prefix}-modal-backdrop` div (see
// .modal-backdrop/.modal-card in tailwind/input.css) — this wires the one
// behavior they all share (click outside the card closes it) and hands
// back open()/close() for the page's own trigger/submit logic to call.
// Called at top-level parse time by pages that need it (same ordering
// requirement as apiCall above), so it can't be deferred to DOM-ready.
function initModal(prefix) {
  const $backdrop = $('#' + prefix + '-modal-backdrop');
  function open() { $backdrop.removeClass('hidden').addClass('panel-fade'); }
  function close() { $backdrop.addClass('hidden'); }
  $backdrop.on('click', function (e) { if (e.target === this) close(); });
  return { open: open, close: close };
}

$(function () {
  if ($('#mobile-menu-btn').length) {
    $('#mobile-menu-btn').on('click', function (e) {
      e.stopPropagation();
      $('#mobile-menu').slideToggle(150);
      $('#menu-icon-open, #menu-icon-close').toggleClass('hidden');
    });
    $('#mobile-menu').on('click', function (e) { e.stopPropagation(); });
    $(document).on('click', function () {
      $('#mobile-menu').slideUp(150);
      $('#menu-icon-open').removeClass('hidden');
      $('#menu-icon-close').addClass('hidden');
    });
  }

  $('#logout-btn').on('click', function () {
    apiCall('POST', '/api/v1/auth/logout').always(function () { window.location = '/login'; });
  });
});
