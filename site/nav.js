// Keeps the Features button's aria-expanded in step with its popover, and places the popover
// under the button in browsers without CSS anchor positioning.
for (const list of document.querySelectorAll('.dropdown-list')) {
  const trigger = document.querySelector(`[popovertarget="${list.id}"]`);
  list.addEventListener('toggle', (event) => {
    const open = event.newState === 'open';
    trigger.setAttribute('aria-expanded', open ? 'true' : 'false');
    if (!open || CSS.supports('position-area', 'block-end')) return;
    const t = trigger.getBoundingClientRect();
    const size = list.getBoundingClientRect();
    list.style.top = `${t.bottom + 8}px`;
    list.style.left = `${Math.min(Math.max(0, t.right - size.width), innerWidth - size.width)}px`;
  });
}
