// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The closing first-run screen: what Daisy set up, and how far its downloads
// have come. Finishing is never blocked.
import { useEffect, useRef, useState } from 'react';
import {
  tauri, errStr, type AiPlan, type AsrPlan, type OnboardingPlan, type DownloadJob,
} from '../../tauri';
import { explainDownloadError, useDownloadJob, useDownloadsFinished } from '../../downloadQueue';

/** Registry ids the download queue tags its jobs with. */
const LLM_ID = 'gemma-4-e4b-it-q4';
const PARAKEET_ID = 'parakeet-tdt-0.6b-v2-int8';
const SA_ID = 'speechanalyzer';

function aiLine(ai: AiPlan): string {
  if (ai.kind === 'none') {
    return `Off — ${ai.reason}. Cloud providers are available under Settings → Providers, with Nerd Mode on.`;
  }
  return "Daisy's built-in model · fast on this computer";
}

function asrLine(asr: AsrPlan): string {
  return asr.engine === 'speech_analyzer'
    ? 'Apple speech recognition'
    : 'Parakeet, on this computer';
}

/** Where a row's model stands. Setup queues a job for anything missing, so a
 *  row with no job has its model already. */
function statusText(job: DownloadJob | undefined, ready: boolean): string {
  if (job === undefined) return ready ? 'ready' : 'queued';
  switch (job.phase.state) {
    case 'downloading': return `downloading ${Math.round(job.phase.pct * 100)}%`;
    case 'warming': return 'preparing…';
    case 'retrying': return `retrying, attempt ${job.phase.attempt} of ${job.phase.of}`;
    case 'failed': return `failed: ${explainDownloadError(job.phase.error).cause}`;
    default: return 'queued';
  }
}

export function PlanStep({ onFinish }: { onFinish: () => void }) {
  const [plan, setPlan] = useState<OnboardingPlan | null>(null);
  const [applyErr, setApplyErr] = useState<string | null>(null);
  const [applying, setApplying] = useState(true);
  const downloadsFinished = useDownloadsFinished();
  const seenFinished = useRef(downloadsFinished);

  function apply() {
    setApplyErr(null);
    setApplying(true);
    tauri.onboardingApply().then(setPlan).catch((e) => {
      setApplyErr(errStr(e));
      // The rows render from the plan either way; the busy label holds until
      // this one lands.
      return tauri.onboardingPlan().then(setPlan).catch(() => {});
    }).finally(() => setApplying(false));
  }

  useEffect(() => {
    apply();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A job that finishes leaves the download queue; re-read the plan so the
  // row reflects the fresh installed/ready flags instead of the ones it was
  // mounted with.
  useEffect(() => {
    if (downloadsFinished === seenFinished.current) return;
    seenFinished.current = downloadsFinished;
    tauri.onboardingPlan().then(setPlan).catch(() => {});
  }, [downloadsFinished]);

  const aiJob = useDownloadJob(LLM_ID);
  const asrJob = useDownloadJob(
    plan?.transcription.engine === 'speech_analyzer' ? SA_ID : PARAKEET_ID,
  );

  return (
    <div style={{ maxWidth: 560 }}>
      <h2 className="h2" style={{ marginTop: 0 }}>Here's how Daisy is set up</h2>
      {!plan && applying && (
        <p className="meta">Checking what this computer can run…</p>
      )}
      {plan && (
        <>
          <div className="plan-row" data-testid="plan-row-ai">
            <span className="plan-row__what">AI summaries</span>
            <span className="plan-row__detail">{aiLine(plan.ai)}</span>
            {plan.ai.kind === 'builtin' && (
              <span className="plan-row__status" data-testid="plan-status-ai">
                {statusText(aiJob, plan.ai.installed)}
              </span>
            )}
          </div>
          <div className="plan-row" data-testid="plan-row-transcription">
            <span className="plan-row__what">Transcription</span>
            <span className="plan-row__detail">{asrLine(plan.transcription)}</span>
            <span className="plan-row__status" data-testid="plan-status-transcription">
              {statusText(
                asrJob,
                plan.transcription.engine === 'speech_analyzer'
                  ? plan.transcription.ready
                  : plan.transcription.installed,
              )}
            </span>
          </div>
          <div className="plan-row" data-testid="plan-row-recordings">
            <span className="plan-row__what">Recordings</span>
            <span className="plan-row__detail">{plan.profile_dir}</span>
          </div>
          <p className="meta" style={{ marginTop: 16 }}>
            Downloads continue in the background, even if you close Daisy.
          </p>
          <p className="meta">
            To use a cloud provider, turn on Nerd Mode in Settings → Display, then open Providers.
          </p>
        </>
      )}
      {applyErr && (
        <div style={{ marginTop: 16 }}>
          <p className="meta meta--danger">{applyErr}</p>
          <button data-testid="plan-retry" className="btn" onClick={apply}>Try again</button>
        </div>
      )}
      <button
        data-testid="wizard-finish"
        className="btn btn--primary"
        style={{ marginTop: 20 }}
        onClick={onFinish}
      >
        {applying ? 'Setting up…' : 'Start using Daisy'}
      </button>
    </div>
  );
}
