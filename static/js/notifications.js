// Shared notification bell + Web Push registration (09-notifications.md),
// included on every authenticated page. Self-initializing — a page only
// needs the bell markup (#notif-bell-btn etc.) and this script tag.
(function () {
  function getCookie(name) {
    const match = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
    return match ? decodeURIComponent(match[1]) : null;
  }

  function apiCall(method, path, body) {
    return $.ajax({
      url: path,
      method: method,
      contentType: 'application/json',
      headers: { 'X-CSRF-Token': getCookie('ocl_csrf') || '' },
      data: body !== undefined ? JSON.stringify(body) : undefined,
    });
  }

  function renderNotification(n) {
    const $item = $('<a>')
      .attr('href', n.link_path || '#')
      .addClass(
        'block px-3 py-2 text-xs border-b border-slate-800 last:border-b-0 hover:bg-slate-800/60 ' +
          (n.read_at ? 'text-slate-500' : 'text-slate-200')
      );
    $item.append($('<p class="font-medium">').text(n.title));
    if (n.body) {
      $item.append($('<p class="text-slate-500 mt-0.5">').text(n.body));
    }
    $item.on('click', function (e) {
      e.preventDefault();
      const target = n.link_path;
      if (n.read_at) {
        if (target) {
          window.location.href = target;
        }
        return;
      }
      // Navigating immediately would cancel an in-flight PATCH before
      // the browser sends it — wait for it (success or failure) first.
      apiCall('PATCH', '/api/v1/notifications/' + n.id + '/read').always(function () {
        if (target) {
          window.location.href = target;
        }
      });
    });
    return $item;
  }

  function loadNotifications() {
    apiCall('GET', '/api/v1/notifications').done(function (list) {
      const $panelList = $('#notif-panel-list').empty();
      const unread = list.filter(function (n) {
        return !n.read_at;
      }).length;
      const $badge = $('#notif-badge');
      if (unread > 0) {
        $badge.text(unread > 9 ? '9+' : String(unread)).removeClass('hidden');
      } else {
        $badge.addClass('hidden');
      }
      if (list.length === 0) {
        $panelList.append(
          $('<p class="px-3 py-4 text-xs text-slate-500 text-center">').text('No notifications yet.')
        );
        return;
      }
      list.slice(0, 20).forEach(function (n) {
        $panelList.append(renderNotification(n));
      });
    });
  }

  // Impossible-to-miss escalated link-end-request banner
  // (06-future-extensions.md §2) — checked on every authenticated
  // page load via this shared script, not just a dismissible
  // notification. Keyholder-only, so this gates on the caller's own
  // role first rather than firing the (guaranteed-403-for-a-
  // submissive) request unconditionally — a 403 would still be
  // handled fine by `.fail()`, but it'd also show up as a "failed to
  // load resource" line in the browser console on every single
  // submissive page load, which this avoids entirely.
  function initEndRequestBanner() {
    apiCall('GET', '/api/v1/auth/me').done(function (me) {
      if (me.role !== 'keyholder') {
        return;
      }
      loadEndRequestBanner();
    });
  }

  function loadEndRequestBanner() {
    apiCall('GET', '/api/v1/keyholder/link-end-requests')
      .done(function (list) {
        const escalated = (list || []).filter(function (r) {
          return r.escalated_at;
        });
        if (escalated.length === 0) {
          return;
        }
        const names = escalated
          .map(function (r) {
            return r.submissive_display_name;
          })
          .join(', ');
        const $banner = $('<div>')
          .attr('id', 'end-request-escalation-banner')
          .attr(
            'class',
            'bg-red-950 border-b border-red-800 text-red-200 text-sm px-4 py-2.5 flex items-center justify-center gap-2 text-center'
          )
          .append(
            $('<span>').text(
              (escalated.length === 1 ? names : escalated.length + ' submissives') +
                ' requested to end the link over a week ago and you haven’t responded yet.'
            )
          )
          .append(
            $('<a href="/dashboard" class="underline font-semibold shrink-0">').text('Review now →')
          );
        $('body').prepend($banner);
      })
      .fail(function () {
        // Only reachable after the role check above already passed —
        // a network hiccup here just means no banner this load.
      });
  }

  function initBell() {
    if ($('#notif-bell-btn').length === 0) {
      return;
    }
    loadNotifications();
    setInterval(loadNotifications, 30000);
    $('#notif-bell-btn').on('click', function (e) {
      e.stopPropagation();
      $('#notif-panel').toggleClass('hidden');
    });
    $('#notif-mark-all-btn').on('click', function () {
      apiCall('PATCH', '/api/v1/notifications/read-all').done(loadNotifications);
    });
    $(document).on('click', function (e) {
      if (!$(e.target).closest('#notif-panel, #notif-bell-btn').length) {
        $('#notif-panel').addClass('hidden');
      }
    });
  }

  function base64UrlToUint8Array(base64String) {
    const padding = '='.repeat((4 - (base64String.length % 4)) % 4);
    const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/');
    const rawData = atob(base64);
    const outputArray = new Uint8Array(rawData.length);
    for (let i = 0; i < rawData.length; ++i) {
      outputArray[i] = rawData.charCodeAt(i);
    }
    return outputArray;
  }

  function registerSubscription(subscription) {
    const json = subscription.toJSON();
    apiCall('POST', '/api/v1/notifications/push-subscriptions', {
      endpoint: json.endpoint,
      keys: json.keys,
      user_agent: navigator.userAgent,
    });
  }

  function subscribeIfNeeded(registration) {
    registration.pushManager.getSubscription().then(function (existing) {
      if (existing) {
        registerSubscription(existing);
        return;
      }
      apiCall('GET', '/api/v1/notifications/vapid-public-key').done(function (res) {
        registration.pushManager
          .subscribe({
            userVisibleOnly: true,
            applicationServerKey: base64UrlToUint8Array(res.public_key),
          })
          .then(registerSubscription)
          .catch(function () {
            // Permission granted but subscribe failed (e.g. no push
            // service reachable) — nothing to do, the in-app feed
            // still works regardless (09-notifications.md).
          });
      });
    });
  }

  function initPushPrompt() {
    if (!('serviceWorker' in navigator) || !('PushManager' in window)) {
      return;
    }
    navigator.serviceWorker
      .register('/static/js/sw.js')
      .then(function (registration) {
        if (Notification.permission === 'denied') {
          return;
        }
        if (Notification.permission === 'granted') {
          subscribeIfNeeded(registration);
          return;
        }
        $('#notif-enable-push-btn')
          .removeClass('hidden')
          .on('click', function () {
            Notification.requestPermission().then(function (permission) {
              $('#notif-enable-push-btn').addClass('hidden');
              if (permission === 'granted') {
                subscribeIfNeeded(registration);
              }
            });
          });
      })
      .catch(function () {
        // Service worker registration can fail (e.g. served over
        // plain HTTP in dev) — push is best-effort, the feed doesn't
        // depend on it.
      });
  }

  $(function () {
    initBell();
    initPushPrompt();
    initEndRequestBanner();
  });
})();
