/**
 * External links, opened in the real browser.
 *
 * The webview has no tabs, no back button and no address bar, so following a link inside
 * it is a trap: the graph editor would be replaced by a web page with no way back. The
 * webview also refuses the navigation outright in a release build, which is how this was
 * first noticed — React Flow's "React Flow" attribution badge simply did nothing.
 *
 * So every link out of the app is intercepted here and handed to the system browser.
 * Which URLs are allowed to leave is *not* decided here: the capability in
 * `src-tauri/capabilities/default.json` holds that list, and a link outside it is refused
 * by the backend. This file only decides that a click on an external link is a request to
 * leave, never a navigation.
 */
import { openUrl } from '@tauri-apps/plugin-opener';

import { IN_APP } from './api';

/** Links we hand off. Anything same-origin stays in the webview and is handled by React. */
const EXTERNAL = /^https?:\/\//i;

function onClick(event: MouseEvent) {
  // Let modified clicks alone: they already mean something to the webview, and none of
  // those meanings is "navigate this window".
  if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey) return;

  const anchor = (event.target as Element | null)?.closest?.('a[href]');
  if (!(anchor instanceof HTMLAnchorElement)) return;

  const href = anchor.href;
  if (!EXTERNAL.test(href) || anchor.origin === window.location.origin) return;

  event.preventDefault();
  openUrl(href).catch((err) => {
    // A refusal here is almost always a URL missing from the opener scope rather than a
    // browser that failed to start, and it is silent from the user's side, so say where
    // the list lives.
    console.error(
      `could not open ${href} in the system browser: ${err}\n` +
        'if this is a new link, add it to the opener scope in src-tauri/capabilities/default.json',
    );
  });
}

/**
 * Start intercepting external links.
 *
 * Capture phase, so a component that stops propagation on its own links — React Flow's
 * attribution does not, but it is not the only link we will ever render — cannot leave
 * one pointing at a dead end.
 */
export function installExternalLinks(): () => void {
  if (!IN_APP) return () => {};
  document.addEventListener('click', onClick, true);
  return () => document.removeEventListener('click', onClick, true);
}
