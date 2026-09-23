/**
 * Getting started, whatever was here before.
 *
 * Three kinds of machine arrive at this panel and each needs a different first sentence:
 *
 * 1. **Nothing else installed.** There is nothing to take over and nothing to read, so
 *    the useful offer is a working starting point built from the hardware actually
 *    present — not an empty canvas and a manual.
 * 2. **Something installed but not running.** Nothing is fighting us *yet*. There is
 *    tuning work on disk worth reading, and possibly a startup entry that will start a
 *    fight at the next reboot.
 * 3. **Something running right now.** All of the above, plus a rival to stand down,
 *    which [`TakeoverPanel`] handles.
 *
 * # Why this takes the window
 *
 * This was a section of the sidebar, and at 310 px wide every paragraph below — the ones
 * whose entire job is to say what a button will do *before* it is pressed — wrapped into
 * a column too cramped to read. Consent nobody reads is not consent. So it is a modal:
 * the one place in this application that takes the whole window, opened once on a machine
 * with nothing configured yet and afterwards only when asked for. It is still not in the
 * control loop — closing it, or never opening it, changes nothing about cooling.
 *
 * # Everything here is opt-in, and says what it will do first
 *
 * Nothing in this panel happens on its own. Import reads a file and shows a translation;
 * it does not install it. Switching off somebody's startup entry is one button per entry,
 * each naming exactly what it will change. Software that quietly rearranges a machine's
 * startup because it decided it should be in charge is precisely what makes people
 * distrust a program with kernel access.
 *
 * # Fidelity is shown, not hidden
 *
 * Two fan controllers do not share a model, so an import is never wholly faithful. Notes
 * are grouped by how well each piece crossed over, and the ones needing a human decision
 * are put first and left visible rather than collapsed behind a summary. An import that
 * looked clean but quietly dropped a fan curve would be worse than one that refused.
 */
import { useCallback, useEffect, useRef, useState } from 'react';

import {
  type Graph,
  type ImportedProfileDto,
  type MigrationSurvey,
  disableRivalAutostart,
  importForeignConfig,
  migrationSurvey,
  setGraph,
  starterPresets,
} from './api';

type Preset = Awaited<ReturnType<typeof starterPresets>>[number];

/** How each fidelity is introduced, ordered by how much it needs a person. */
const FIDELITY: Record<string, { label: string; tone: string; rank: number }> = {
  'needs-attention': { label: 'Check this', tone: 'warn', rank: 0 },
  skipped: { label: 'Not brought across', tone: 'muted', rank: 1 },
  approximated: { label: 'Close, not exact', tone: 'muted', rank: 2 },
  exact: { label: 'Brought across', tone: 'ok', rank: 3 },
};

interface Props {
  open: boolean;
  onClose: () => void;
  /**
   * A document produced here was accepted by the backend, and this is the sentence the
   * user needs about it. The caller owns both — it re-reads the graph and shows the
   * sentence somewhere the modal is not covering.
   */
  onApplied: (note: string) => void;
}

export default function MigrationPanel({ open, onClose, onApplied }: Props) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [survey, setSurvey] = useState<MigrationSurvey | null>(null);
  const [presets, setPresets] = useState<Preset[] | null>(null);
  const [preview, setPreview] = useState<ImportedProfileDto | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const load = useCallback(async () => {
    const found = await migrationSurvey().catch(() => null);
    setSurvey(found);
    // Always offered, including when there is something to import. Somebody who has an
    // old configuration and does not want it back still needs a starting point, and
    // gating the presets on a clean machine left exactly that person with nothing.
    setPresets(await starterPresets().catch(() => null));
  }, []);

  // Surveying means walking the registry and the filesystem, so it happens on open rather
  // than on mount: reopening re-reads the machine instead of showing whatever was true
  // when the window first appeared.
  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  // `showModal` is what buys the focus trap, the inert background and Escape, none of
  // which are worth reimplementing. React renders the element; the browser opens it.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  }, [open]);

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

  /**
   * Load a document this panel produced, then get out of the way.
   *
   * The outcome sentence is handed to the caller rather than shown in the footer, because
   * what it says is *go and look at the graph* — and this modal is what covers the graph.
   * An instruction you have to dismiss before you can follow it is not much of one.
   *
   * A failure is the opposite case and stays here: nothing was loaded, the editor behind
   * is unchanged, and the reason belongs beside the button that caused it.
   */
  const applyGraph = useCallback(
    async (label: string, graph: Graph, outcome: string) => {
      setBusy(label);
      setNote(null);
      try {
        const result = await setGraph(graph);
        if (!result.ok) {
          throw new Error(result.errors.map((e) => e.message).join('; '));
        }
        // Deliberately no re-survey: it walks the registry and the filesystem to refresh
        // a panel that is closing.
        onApplied(outcome);
      } catch (e) {
        setNote(String(e));
      } finally {
        setBusy(null);
      }
    },
    [onApplied],
  );

  const notes = preview
    ? [...preview.notes].sort(
        (a, b) => (FIDELITY[a.fidelity]?.rank ?? 9) - (FIDELITY[b.fidelity]?.rank ?? 9),
      )
    : [];

  return (
    <dialog
      ref={dialogRef}
      className="modal"
      aria-labelledby="getting-started-title"
      // Escape and the close button arrive here alike, so the parent's idea of whether
      // this is open cannot drift from the browser's.
      onClose={onClose}
      // The backdrop is not a child, so a click on it lands on the dialog element itself.
      // The panel fills the dialog box, which makes that test unambiguous.
      onClick={(e) => {
        if (e.target === dialogRef.current) onClose();
      }}
    >
      <div className="modal__panel">
        <header className="modal__header">
          <h2 id="getting-started-title">Getting started</h2>
          <button type="button" className="modal__close" onClick={onClose} aria-label="Close">
            ×
          </button>
        </header>

        <div className="modal__body takeover takeover--modal">
          {!survey ? (
            <p className="takeover__muted">Looking at what is already on this machine…</p>
          ) : (
            <>
              {survey.nothingElseHere && (
                <p className="takeover__muted">
                  No other fan-control software is running, installed, or set to start with
                  this machine. Nothing needs taking over.
                </p>
              )}

              {/* --- 2. something is set to start with the machine -------------------- */}
              {survey.autostart.length > 0 && (
                <>
                  <h3>Starts with this machine</h3>
                  {/* The point of saying this out loud: a survey that only looked at
                      running programs would have reported all-clear, and the fight would
                      begin at the next reboot with nobody watching. */}
                  <p className="takeover__warn">
                    {survey.autostart.length === 1 ? 'This is' : 'These are'} set to start
                    when Windows does. Even if{' '}
                    {survey.autostart.length === 1 ? 'it is' : 'they are'} not running now,{' '}
                    {survey.autostart.length === 1 ? 'it' : 'they'} will be after a restart —
                    and then two programs will be driving the same fans.
                  </p>

                  <ul className="takeover__apps">
                    {survey.autostart.map((entry) => (
                      <li key={entry.id}>
                        <strong>{entry.name}</strong> — {entry.location}
                        <p>
                          <code>{entry.command}</code>
                        </p>
                        <p>
                          {entry.reversible
                            ? 'Switching this off can be undone: it is disabled rather than deleted.'
                            : 'This one has no "disabled" setting, so it has to be removed. ' +
                              'OpenFan will tell you exactly what it removed, so you can put it back.'}
                        </p>
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            run(`autostart-${entry.id}`, async () => {
                              const done = await disableRivalAutostart(entry.id);
                              setNote(
                                done.restoreHint
                                  ? `Switched off ${done.what}. ${done.restoreHint}`
                                  : `Switched off ${done.what}.`,
                              );
                            })
                          }
                        >
                          {busy === `autostart-${entry.id}`
                            ? 'Working…'
                            : `Stop ${entry.name} starting with Windows`}
                        </button>
                      </li>
                    ))}
                  </ul>
                </>
              )}

              {/* --- 3. there is tuning work on disk worth reading --------------------- */}
              {survey.configs.length > 0 && (
                <>
                  <h3>Bring your settings across</h3>
                  <p className="takeover__muted">
                    Reading a configuration does not change anything. You will see exactly
                    what came across, and what did not, before deciding whether to use it.
                  </p>

                  <ul className="takeover__apps">
                    {survey.configs.map((config) => (
                      <li key={config.path}>
                        <strong>{config.name}</strong> — {config.foundVia}
                        <p>
                          <code>{config.path}</code>
                        </p>
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            run(`import-${config.path}`, async () => {
                              setPreview(await importForeignConfig(config.path));
                            })
                          }
                        >
                          {busy === `import-${config.path}` ? 'Reading…' : 'Read it'}
                        </button>
                      </li>
                    ))}
                  </ul>
                </>
              )}

              {/* --- what an import actually produced ---------------------------------- */}
              {preview && (
                <div className="takeover__result">
                  <h3>{preview.name}</h3>

                  {preview.empty ? (
                    <p className="takeover__warn">
                      Nothing in that configuration could be brought across. The notes below
                      say why for each part of it.
                    </p>
                  ) : (
                    <p className="takeover__ok">
                      This is a translation, not a copy. Read the notes before using it.
                    </p>
                  )}

                  <ul className="takeover__apps takeover__apps--quiet">
                    {notes.map((n, i) => {
                      const style = FIDELITY[n.fidelity] ?? { label: n.fidelity, tone: 'muted' };
                      return (
                        <li key={`${n.subject}-${i}`}>
                          <span className={`takeover__${style.tone}`}>{style.label}</span> —{' '}
                          <strong>{n.subject}</strong>
                          <p>{n.detail}</p>
                        </li>
                      );
                    })}
                  </ul>

                  {/* Measurements another tool already made. Worth calling out separately:
                      this is the one thing in a foreign configuration we cannot reproduce
                      without running somebody's fan down until it stops. */}
                  {preview.calibration.length > 0 && (
                    <>
                      <h3>Fan measurements found</h3>
                      <p className="takeover__muted">
                        Somebody already measured how these fans respond, including where
                        they stop. Keeping that means not having to find it again by slowing
                        a fan until it stalls.
                      </p>
                      <ul className="takeover__apps takeover__apps--quiet">
                        {preview.calibration.map((c) => (
                          <li key={c.channel}>
                            <strong>{c.label}</strong>
                            <p>
                              {c.points.length} measurements.{' '}
                              {c.foundTheStall && c.lowestTurningDuty !== null
                                ? `Stops below about ${c.lowestTurningDuty} %.`
                                : 'These never recorded the fan stopping, so they do not say ' +
                                  'how slowly it can safely run.'}
                            </p>
                          </li>
                        ))}
                      </ul>
                    </>
                  )}

                  {!preview.empty && (
                    <div className="takeover__action">
                      <p className="takeover__consequence">
                        This loads the translated configuration into the editor. It does not
                        start driving anything — fan control stays off until you turn it on.
                      </p>
                      <div className="takeover__buttons">
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            applyGraph(
                              'apply-import',
                              preview.graph,
                              'Loaded. Check the temperature each fan follows before turning control on.',
                            )
                          }
                        >
                          {busy === 'apply-import' ? 'Loading…' : 'Use this configuration'}
                        </button>
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() => setPreview(null)}
                        >
                          Discard
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              )}

              {/* Offered last, because somebody with a configuration to bring across should
                  be shown that first — but offered in every state, since declining an
                  import still leaves a person needing somewhere to start. */}
              {presets && presets.length > 0 && !preview && (
                <>
                  <h3>{survey.nothingElseHere ? 'Start from' : 'Or start fresh'}</h3>
                  <p className="takeover__muted">
                    Built from the fans and sensors this machine actually reports. Each one
                    is a complete, working configuration you can then change.
                  </p>
                  <ul className="takeover__apps">
                    {presets.map((preset) => (
                      <li key={preset.id}>
                        <strong>{preset.name}</strong>
                        <p>{preset.description}</p>
                        <button
                          type="button"
                          disabled={busy !== null}
                          onClick={() =>
                            applyGraph(
                              `preset-${preset.id}`,
                              preset.graph,
                              `Started from "${preset.name}". Nothing drives a fan until you turn control on.`,
                            )
                          }
                        >
                          {busy === `preset-${preset.id}` ? 'Loading…' : `Use ${preset.name}`}
                        </button>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </>
          )}
        </div>

        <footer className="modal__footer">
          {/* What lands here is the outcome of everything that leaves you in this modal:
              a startup entry switched off, or an apply that failed. It sits at the bottom
              rather than moving with the section that produced it, because an outcome
              that scrolls out of sight is one nobody reads. */}
          {note && <p className="modal__note">{note}</p>}
          <div className="modal__footer-buttons">
            <button
              type="button"
              className="button"
              disabled={busy !== null}
              onClick={() => run('refresh', load)}
            >
              {busy === 'refresh' ? 'Checking…' : 'Check again'}
            </button>
            <button type="button" className="button" onClick={onClose}>
              Close
            </button>
          </div>
        </footer>
      </div>
    </dialog>
  );
}
