// Service worker for Web Push delivery (09-notifications.md §1). Only
// job: show the native notification the push payload describes, and
// deep-link into the app on click. No caching/offline behavior —
// this app isn't a full PWA, just a push delivery target.

self.addEventListener('push', function (event) {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch (e) {
    data = {};
  }
  const title = data.title || "Owner's Cock Ledger";
  const options = {
    body: data.body || '',
    icon: '/static/images/favicon.png',
    data: { link_path: data.link_path || '/' },
  };
  event.waitUntil(self.registration.showNotification(title, options));
});

self.addEventListener('notificationclick', function (event) {
  event.notification.close();
  const linkPath = (event.notification.data && event.notification.data.link_path) || '/';
  event.waitUntil(
    clients.matchAll({ type: 'window', includeUncontrolled: true }).then(function (clientList) {
      for (const client of clientList) {
        if ('focus' in client) {
          client.navigate(linkPath);
          return client.focus();
        }
      }
      if (clients.openWindow) {
        return clients.openWindow(linkPath);
      }
    })
  );
});
