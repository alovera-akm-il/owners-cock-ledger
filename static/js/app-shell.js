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
