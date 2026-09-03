// Shared live countdown for "locked until" digital-clock displays.
// Renders into any element with [data-target-epoch], using the
// days/hrs/min/sec digit-block markup, and updates it every second.
// The remaining time is computed from data-target-epoch minus
// data-server-now-epoch (the server's clock at page-render time) plus
// elapsed client time since load — this corrects for client/server
// clock skew rather than trusting the browser's own clock outright.
function renderCountdownBlock(container) {
  const targetEpoch = parseInt(container.dataset.targetEpoch, 10);
  const serverNowEpoch = parseInt(container.dataset.serverNowEpoch, 10);
  const loadedAtClientMs = Date.now();

  const daysEl = container.querySelector('[data-cd="days"]');
  const hoursEl = container.querySelector('[data-cd="hours"]');
  const minutesEl = container.querySelector('[data-cd="minutes"]');
  const secondsEl = container.querySelector('[data-cd="seconds"]');
  const overdueLabel = container.querySelector('[data-cd="overdue-label"]');
  const digitEls = [daysEl, hoursEl, minutesEl, secondsEl].filter(Boolean);

  function tick() {
    const elapsedSinceLoad = (Date.now() - loadedAtClientMs) / 1000;
    const remaining = (targetEpoch - serverNowEpoch) - elapsedSinceLoad;
    const overdue = remaining <= 0;
    let diff = Math.round(Math.abs(remaining));
    const days = Math.floor(diff / 86400); diff -= days * 86400;
    const hours = Math.floor(diff / 3600); diff -= hours * 3600;
    const minutes = Math.floor(diff / 60); diff -= minutes * 60;
    const seconds = diff;

    const pad = (n) => String(n).padStart(2, '0');
    if (daysEl) daysEl.textContent = pad(days);
    if (hoursEl) hoursEl.textContent = pad(hours);
    if (minutesEl) minutesEl.textContent = pad(minutes);
    if (secondsEl) secondsEl.textContent = pad(seconds);

    if (overdueLabel) overdueLabel.classList.toggle('hidden', !overdue);
    digitEls.forEach((el) => el.classList.toggle('text-red-400', overdue));
  }

  tick();
  return setInterval(tick, 1000);
}

$(function () {
  $('[data-target-epoch]').each(function () { renderCountdownBlock(this); });
});
