// The self-routing half of options-hash.html: an options page that picks its own sub-route on load.
// An extension page may not run inline script, so this is a file.
if (!location.hash) location.replace(`${location.pathname}#general`);
