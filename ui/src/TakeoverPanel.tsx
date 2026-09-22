/**
 * Explains what stands between OpenFan and this machine's fans, and offers to clear it.
 *
 * The panel exists because the alternative is worse than useless: without it, OpenFan
 * silently shares a fan header with another program, both write every tick, the fan obeys
 * whichever wrote last, and each application's interface confidently shows the duty it
 * believes it commanded.
 *
 * Two rules shape what it says.
 *
 * **Never claim more certainty than the backend has.** A channel whose control is
 * `unknown` means the backend could not tell, which is *not* a synonym for "fine" and is
 * never rendered as one. A channel that is `foreign` might be actively driven by another
 * program or might have been abandoned in manual by one that already exited — those look
 * identical from a single reading and the panel does not pretend otherwise.
 *
 * **Say what will happen before it happens, not after.** A takeover changes how the
 * machine's fans behave: headers move to the board's own curve, which may be louder or
 * quieter than whatever the user had configured elsewhere. That belongs in front of the
 * button, not in the result.
 */
import { useCallback, useEffect, useState } from 'react';

import {
  type ChannelControlDto,
  type ContentionReport,
  type TakeoverResult,
  contentionReport,
  takeOver,
} from './api';

/** How the panel describes each control state, in the user's terms rather than ours. */
const CONTROL_TEXT: Record<ChannelControlDto, { label: string; tone: string; detail: string }> = {
  firmware: {
    label: 'Board firmware',
    tone: 'ok',
    detail: "The motherboard's own fan curve is handling this header.",
  },
  ours: {
    label: 'OpenFan',
    tone: 'ok',
    detail: 'OpenFan has taken this header and is responsible for it.',
  },
  foreign: {
    label: 'Another program',
    tone: 'warn',
    detail:
      'Set to manual by something that is not OpenFan. Either another program is driving ' +
      'it, or one left it this way and nothing is responding to temperature on it.',
  },
  unknown: {
    label: 'Unknown',
    tone: 'warn',
    detail: 'This backend cannot tell who is driving this header.',
  },
};

export default function TakeoverPanel() {
  const [report, setReport] = useState<ContentionReport | null>(null);
  const [result, setResult] = useState<TakeoverResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [force, setForce] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setReport(await contentionReport());
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const run = useCallback(async () => {
    setBusy(true);
    setResult(null);
    setError(null);
    try {
      const outcome = await takeOver(force);
      setResult(outcome);
      setReport(outcome.report);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [force]);

  if (error && !report) {
    return (
      <section className="takeover">
        <h2>Fan control</h2>
        <p className="takeover__error">{error}</p>
      </section>
    );
  }

  if (!report) {
    return (
      <section className="takeover">
        <h2>Fan control</h2>
        <p className="takeover__muted">Checking…</p>
      </section>
    );
  }

  const mustStop = report.apps.filter((a) => a.mustStop);
  const canCoexist = report.apps.filter((a) => !a.mustStop);

  return (
    <section className="takeover">
      <h2>Fan control</h2>

      {report.clear ? (
        <p className="takeover__ok">
          Nothing is in the way. Every header is under the board firmware or under OpenFan.
        </p>
      ) : (
        <p className="takeover__warn">
          {report.stranded.length > 0
            ? `${report.stranded.length} header${report.stranded.length === 1 ? '' : 's'} ${
                report.stranded.length === 1 ? 'is' : 'are'
              } set to manual by something other than OpenFan.`
            : 'Another fan controller is running.'}
        </p>
      )}

      <ul className="takeover__channels">
        {report.channels.map((c) => {
          const text = CONTROL_TEXT[c.control];
          return (
            <li key={c.id} className={`takeover__channel takeover__channel--${text.tone}`}>
              <span className="takeover__channel-name">{c.label}</span>
              <span className="takeover__channel-state" title={text.detail}>
                {text.label}
              </span>
            </li>
          );
        })}
      </ul>

      {mustStop.length > 0 && (
        <>
          <h3>Must stand down</h3>
          <ul className="takeover__apps">
            {mustStop.map((a) => (
              <li key={`${a.key}-${a.pid}`}>
                <strong>{a.name}</strong> <span className="takeover__muted">({a.processName})</span>
                <p>{a.note}</p>
              </li>
            ))}
          </ul>
        </>
      )}

      {canCoexist.length > 0 && (
        <>
          <h3>Can stay</h3>
          <ul className="takeover__apps takeover__apps--quiet">
            {canCoexist.map((a) => (
              <li key={`${a.key}-${a.pid}`}>
                <strong>{a.name}</strong> <span className="takeover__muted">({a.processName})</span>
                <p>{a.note}</p>
              </li>
            ))}
          </ul>
        </>
      )}

      {!report.clear && (
        <div className="takeover__action">
          {/* Said before the button, not after: a takeover changes how the machine's fans
              behave, and a user who finds that out afterwards has been ambushed. */}
          <p className="takeover__consequence">
            This will close {mustStop.length > 0 ? mustStop.map((a) => a.name).join(', ') : 'nothing'}
            {report.stranded.length > 0 &&
              ` and return ${report.stranded.length} header${
                report.stranded.length === 1 ? '' : 's'
              } to the motherboard's own fan curve`}
            . Your fans may get louder or quieter than whatever you had configured elsewhere.
          </p>

          <label className="takeover__force">
            <input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} />
            Force-close anything that will not exit
            <span className="takeover__muted">
              {' '}
              — a terminated program runs no shutdown code, so it restores nothing it was
              controlling.
            </span>
          </label>

          <button type="button" onClick={run} disabled={busy}>
            {busy ? 'Taking over…' : 'Take over fan control'}
          </button>
        </div>
      )}

      {error && <p className="takeover__error">{error}</p>}

      {result && (
        <div className={`takeover__result takeover__result--${result.succeeded ? 'ok' : 'warn'}`}>
          <h3>{result.succeeded ? 'Done' : 'Not finished'}</h3>
          <ol>
            {result.steps.map((step) => (
              <li key={step}>{step}</li>
            ))}
          </ol>
          {result.blocker && <p className="takeover__warn">{result.blocker}</p>}
        </div>
      )}

      <button type="button" className="takeover__refresh" onClick={refresh} disabled={busy}>
        Re-check
      </button>
    </section>
  );
}
