// Shared live countdown for "supposed to be locked until" displays.
// Renders into any element with [data-countdown="ISO-timestamp"] using
// the days/hrs/min/sec digit-block markup below, and updates it every second.
function renderCountdownBlock(container) {
  const targetIso = container.getAttribute('data-countdown');
  const target = new Date(targetIso).getTime();

  function tick() {
    const now = Date.now();
    const overdue = now > target;
    let diff = Math.abs(target - now);
    const days = Math.floor(diff / 86400000); diff -= days * 86400000;
    const hours = Math.floor(diff / 3600000); diff -= hours * 3600000;
    const minutes = Math.floor(diff / 60000); diff -= minutes * 60000;
    const seconds = Math.floor(diff / 1000);

    const pad = (n) => String(n).padStart(2, '0');
    container.querySelector('[data-cd="days"]').textContent = pad(days);
    container.querySelector('[data-cd="hours"]').textContent = pad(hours);
    container.querySelector('[data-cd="minutes"]').textContent = pad(minutes);
    container.querySelector('[data-cd="seconds"]').textContent = pad(seconds);

    const label = container.querySelector('[data-cd="overdue-label"]');
    if (label) label.classList.toggle('hidden', !overdue);
    container.classList.toggle('countdown-overdue', overdue);
  }

  tick();
  return setInterval(tick, 1000);
}

$(function () {
  $('[data-countdown]').each(function () { renderCountdownBlock(this); });
});
