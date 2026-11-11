// Small progressive enhancements. Everything works without this file.
(function () {
  // Practice test countdown: reads data-deadline (ms since epoch) and updates every second.
  function tick() {
    document.querySelectorAll('[data-deadline]').forEach(function (el) {
      var left = Math.max(0, Math.floor((Number(el.dataset.deadline) - Date.now()) / 1000));
      var m = Math.floor(left / 60), s = left % 60;
      el.textContent = m + ':' + (s < 10 ? '0' : '') + s;
      el.setAttribute('aria-label', 'Time left ' + m + ' minutes ' + s + ' seconds');
      var warn = document.getElementById('five-min');
      if (warn && left <= 300 && left > 0) warn.hidden = false;
    });
  }
  setInterval(tick, 1000);
  document.addEventListener('DOMContentLoaded', tick);
  // Hide or show the timer without losing time.
  document.addEventListener('click', function (e) {
    var b = e.target.closest('[data-toggle-timer]');
    if (!b) return;
    var t = document.getElementById('timer');
    if (!t) return;
    t.hidden = !t.hidden;
    b.textContent = t.hidden ? 'Show timer' : 'Hide timer';
  });
  // Move focus to new feedback or questions after an htmx swap (htmx 4 event name).
  document.addEventListener('htmx:after:swap', function () {
    var f = document.querySelector('[data-focus]');
    if (f) { f.setAttribute('tabindex', '-1'); f.focus(); f.removeAttribute('data-focus'); }
  });
})();
