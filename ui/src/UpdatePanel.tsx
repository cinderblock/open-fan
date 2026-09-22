/**
 * Updating OpenFan.
 *
 * Two ways to install, and the difference is *who runs the installer*:
 *
 * - **Update now** — the window launches it, so Windows raises one administrator prompt.
 * - **Install updates automatically** — the background service installs them itself, with
 *   no prompt at all, because it already runs with the rights to do it.
 *
 * The second is more convenient and more dangerous, and the interface says so rather than
 * burying it. A service that installs code unattended is the most powerful thing in this
 * product; someone turning that on should know what they are turning on. It is off until
 * they do.
 *
 * Both paths install the same artifact, and the service downloads and signature-checks it
 * either way. Verification never moves into the window — an unelevated process could have
 * the file substituted underneath it.
 */
import { useCallback, useEffect, useState } from 'react';

import {
  type UpdateStatus,
  applyUpdatePrompted,
  applyUpdateSilently,
  checkForUpdate,
  setAutoUpdate,
  updateStatus,
} from './api';

export default function UpdatePanel() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const load = useCallback(async () => {
    setStatus(await updateStatus().catch(() => null));
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = useCallback(
    async (label: string, action: () => Promise<unknown>, after?: string) => {
      setBusy(label);
      setNote(null);
      try {
        await action();
        if (after) setNote(after);
        await load();
      } catch (e) {
        setNote(String(e));
      } finally {
        setBusy(null);
      }
    },
    [load],
  );

  if (!status) return null;

  const available = status.available !== null && status.available !== undefined;

  return (
    <section className="takeover">
      <h2>Updates</h2>

      <p className="takeover__muted">Version {status.currentVersion}</p>

      {/* Stated plainly rather than hidden: with no signing key, nothing can be verified,
          so neither path will install. Silently doing nothing would be worse. */}
      {!status.verifiable && (
        <p className="takeover__warn">
          This build has no update signing key, so updates cannot be verified and will not
          be installed.
        </p>
      )}

      {status.error && <p className="takeover__error">{status.error}</p>}

      {available ? (
        <>
          <p className="takeover__ok">Version {status.available} is available.</p>
          {status.notes && <p className="takeover__muted">{status.notes}</p>}
        </>
      ) : (
        <p className="takeover__muted">
          {status.rejected ?? 'No update available.'}
        </p>
      )}

      <div className="takeover__action">
        <button
          type="button"
          onClick={() => run('check', checkForUpdate)}
          disabled={busy !== null}
        >
          {busy === 'check' ? 'Checking…' : 'Check for updates'}
        </button>

        {available && status.verifiable && (
          <>
            <p className="takeover__consequence">
              Installing restarts the background service. Your fans go back to the
              motherboard's own curve for a moment while it does, then OpenFan picks them
              up again.
            </p>

            <button
              type="button"
              onClick={() =>
                run('prompted', applyUpdatePrompted, 'The installer is starting.')
              }
              disabled={busy !== null}
            >
              {busy === 'prompted' ? 'Starting…' : 'Update now'}
            </button>
            <span className="takeover__muted">
              Windows will ask for administrator once.
            </span>

            {status.automatic && (
              <button
                type="button"
                onClick={() =>
                  run('silent', applyUpdateSilently, 'Installing in the background.')
                }
                disabled={busy !== null}
              >
                {busy === 'silent' ? 'Installing…' : 'Install in the background'}
              </button>
            )}
          </>
        )}

        <label className="takeover__force">
          <input
            type="checkbox"
            checked={status.automatic}
            disabled={busy !== null}
            onChange={(e) => run('auto', () => setAutoUpdate(e.target.checked))}
          />
          Install updates automatically, without asking
          <span className="takeover__muted">
            {' '}
            — the background service installs them itself, so there is no administrator
            prompt. It only ever installs OpenFan releases signed by the project, and
            never an older version than the one you have.
          </span>
        </label>
      </div>

      {note && <p className="takeover__muted">{note}</p>}
    </section>
  );
}
