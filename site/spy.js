// Marks the side index link for the section in view. Browsers with scroll-target-group style it
// through :target-current, so there the observer only keeps aria-current in step; the rest also
// get the .target-current class.
const links = [...document.querySelectorAll('.toc a')];
const native = CSS.supports('scroll-target-group: auto');

const observer = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    for (const link of links) {
      const current = link.hash === `#${entry.target.id}`;
      link.setAttribute('aria-current', current ? 'true' : 'false');
      if (!native) link.classList.toggle('target-current', current);
    }
  }
}, { rootMargin: '-40% 0px -55% 0px' });

for (const link of links) {
  const section = document.querySelector(link.hash);
  if (section) observer.observe(section);
}
