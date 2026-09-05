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

// Renders one custom check-in field's input, shared between
// submit_checkin.html (the non-live free-choice form) and
// checkin_live.html (the live-synced form) — the two call sites differ
// in exactly two ways, both passed in via `opts` rather than a free
// variable this function would otherwise have to assume:
//   - `opts.devicesPath`: a `select`-type field backed by
//     `config.source === 'devices'` needs to know where to fetch the
//     submissive's device list from. submit_checkin.html always has the
//     submissive id up front (from the page's own data attribute or the
//     logged-in submissive); checkin_live.html only learns it once the
//     play session's own API response comes back, so it can't be a
//     plain top-level constant there.
//   - `opts.onPhotoPicked`/`opts.onAudioPicked`: once a check-in already
//     exists, checkin_live.html uploads a newly-picked photo/audio file
//     immediately (there's no separate "submit" step left to ride along
//     with); submit_checkin.html has no live check-in to upload to yet,
//     so it just omits these and the file rides along with the eventual
//     multipart create request instead.
function fieldInput(f, opts) {
  opts = opts || {};
  const $wrap = $('<div>');
  $wrap.append($('<label class="block text-xs font-medium text-slate-400 mb-1">').text(f.label));
  if (f.description) $wrap.append($('<p class="text-[11px] text-slate-500 mb-1.5">').text(f.description));

  let $input;
  if (f.field_type === 'select') {
    $input = $('<select class="w-full bg-slate-950 border border-slate-800 rounded-lg text-sm p-2 text-slate-200">').attr('data-key', f.field_key);
    if (f.config && f.config.source === 'devices') {
      apiCall('GET', opts.devicesPath).done(function (devices) {
        devices.filter(d => !d.retired_at).forEach(function (d) {
          $input.append($('<option>').val(d.name).text(d.name));
        });
      });
    } else {
      (f.config && f.config.options || []).forEach(function (opt) {
        $input.append($('<option>').val(opt).text(opt));
      });
    }
  } else if (f.field_type === 'scale') {
    const min = (f.config && f.config.min) ?? 1;
    const max = (f.config && f.config.max) ?? 5;
    const mid = Math.round((min + max) / 2);
    $input = $('<input type="range" class="w-full accent-amber-500">').attr({ 'data-key': f.field_key, min: min, max: max, value: mid });
    const $out = $('<span class="text-sm font-mono text-amber-400">').text(mid);
    const $labelRow = $('<div class="flex items-center justify-between mb-1">');
    $wrap.find('label').detach().appendTo($labelRow);
    $labelRow.append($out);
    $wrap.prepend($labelRow);
    $input.on('input', function () { $out.text($(this).val()); });
    const $minMax = $('<div class="flex justify-between text-[10px] text-slate-600 mt-0.5">')
      .append($('<span>').text((f.config && f.config.min_label) || min))
      .append($('<span>').text((f.config && f.config.max_label) || max));
    $wrap.append($input, $minMax);
    return $wrap;
  } else if (f.field_type === 'number') {
    $input = $('<input type="number" class="w-full bg-slate-950 border border-slate-800 rounded-lg text-sm p-2 text-slate-200">').attr('data-key', f.field_key);
    if (f.config && f.config.unit) $wrap.find('label').append($('<span class="text-slate-600 font-normal">').text(' (' + f.config.unit + ')'));
  } else if (f.field_type === 'boolean') {
    $input = $('<select class="w-full bg-slate-950 border border-slate-800 rounded-lg text-sm p-2 text-slate-200">').attr('data-key', f.field_key);
    $input.append($('<option value="false">').text('No'), $('<option value="true">').text('Yes'));
  } else if (f.field_type === 'photo') {
    // Not part of field_values (a file can't be a JSON scalar) — the
    // submit handler pulls this element's .files[0] out separately by
    // its data-photo-field marker and sends it as a multipart 'photo'
    // part alongside the rest of the request. Accepts a video too — the
    // preview swaps between <img> and <video> based on the picked
    // file's actual type.
    $input = $('<input type="file" accept="image/png,image/jpeg,video/mp4,video/webm" class="block w-full text-xs text-slate-400 file:mr-3 file:rounded-lg file:border-0 file:bg-slate-800 file:px-3 file:py-2 file:text-xs file:font-semibold file:text-slate-200 hover:file:bg-slate-700">').attr({ 'data-key': f.field_key, 'data-photo-field': '1' });
    const $imgPreview = $('<img class="hidden mt-2 max-h-40 rounded-lg border border-slate-800">');
    const $videoPreview = $('<video controls class="hidden mt-2 max-h-40 rounded-lg border border-slate-800">');
    $input.on('change', function () {
      const file = this.files[0];
      $imgPreview.addClass('hidden');
      $videoPreview.addClass('hidden');
      if (!file) return;
      const url = URL.createObjectURL(file);
      if (file.type.startsWith('video/')) {
        $videoPreview.attr('src', url).removeClass('hidden');
      } else {
        $imgPreview.attr('src', url).removeClass('hidden');
      }
      if (opts.onPhotoPicked) opts.onPhotoPicked(file);
    });
    $wrap.append($input, $imgPreview, $videoPreview);
    return $wrap;
  } else if (f.field_type === 'audio') {
    // Same idea as the photo/video field, but for the independent
    // voice-memo slot — a multipart 'audio' part.
    $input = $('<input type="file" accept="audio/webm,audio/mp4,audio/mpeg,audio/wav,.mp3,.wav" class="block w-full text-xs text-slate-400 file:mr-3 file:rounded-lg file:border-0 file:bg-slate-800 file:px-3 file:py-2 file:text-xs file:font-semibold file:text-slate-200 hover:file:bg-slate-700">').attr({ 'data-key': f.field_key, 'data-audio-field': '1' });
    const $preview = $('<audio controls class="hidden mt-2 w-full">');
    $input.on('change', function () {
      const file = this.files[0];
      if (!file) { $preview.addClass('hidden'); return; }
      $preview.attr('src', URL.createObjectURL(file)).removeClass('hidden');
      if (opts.onAudioPicked) opts.onAudioPicked(file);
    });
    $wrap.append($input, $preview);
    return $wrap;
  } else {
    $input = $('<textarea rows="2" class="w-full bg-slate-950 border border-slate-800 rounded-lg text-sm p-2 text-slate-200">').attr('data-key', f.field_key);
  }
  $wrap.append($input);
  return $wrap;
}

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
