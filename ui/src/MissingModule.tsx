/**
 * Offer to install the PawnIO hardware module.
 *
 * Shown only when the driver is present and its module is not — the state a fresh install
 * lands in, and one that otherwise looks identical to "this motherboard is unsupported".
 * Telling those apart matters: one is a dead end, the other is one click.
 *
 * The download is deliberately not automatic. A fan controller that reaches out to the
 * network on its own the first time it runs is surprising, and surprising people is how
 * trust in software with kernel access is lost.
 */
import { useCallback, useState } from 'react';

import { fetchHardwareModule } from './api';

export default function MissingModule() {
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<string | null>(null);

  const install = useCallback(async () => {
    setBusy(true);
    setOutcome(null);
    try {
      setOutcome(await fetchHardwareModule());
    } catch (e) {
      setOutcome(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  return (
    <div className="takeover__consequence">
      <p>
        PawnIO is installed, but the hardware module it needs to read this motherboard is
        not — PawnIO ships the driver only. Until it is installed OpenFan cannot see your
        sensors or fans.
      </p>
      <button type="button" onClick={install} disabled={busy}>
        {busy ? 'Downloading…' : 'Install hardware module'}
      </button>
      {outcome && <p className="takeover__muted">{outcome}</p>}
    </div>
  );
}
