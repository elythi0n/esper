import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "@/api/client";
import type { Setting, SettingsAnswer } from "@/api/types";
import { Button } from "@/components/button";
import { DurationInput, NumberInput } from "@/components/form-controls";
import { Input } from "@/components/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/select";
import { SettingRow } from "@/components/setting";
import { Skeleton } from "@/components/states";
import { Switch } from "@/components/switch";
import { useToast } from "@/components/toast";
import { Heading, Trouble } from "./parts";

/**
 * What this machine is *set to*, as against what it is and what it can do.
 *
 * # Why this exists
 *
 * Every one of these was a file somebody edited by hand — and nothing creates
 * that file, so the first change meant knowing the path, the syntax and the
 * key. `esper config` listed thirty-two settings and offered no way to change
 * one of them.
 *
 * # Why it declares nothing about them
 *
 * Not the list, not the order, not the bounds, not the words a choice may be.
 * All of it comes from the engine, which is where the registry is and where
 * the file is validated. A page that restated any of it would be a second
 * list to keep in step, and the way that fails is a control offering a value
 * the file refuses.
 *
 * Even the grouping follows the engine: the settings arrive in the order
 * somebody would read them — what this machine *is*, then what it serves,
 * then how it is found, then how it is reached — and a heading appears
 * wherever the part before the dot changes.
 */

/** What a person calls each part of a key. */
const GROUP_NAMES: Readonly<Record<string, string>> = {
  device: "Naming",
  host: "Serving a session",
  policy: "Policy",
  consent: "Being asked",
  session: "While a session runs",
  audit: "The record",
  forward: "Forwarding a port",
  files: "Files",
  discovery: "Being found",
  connectivity: "Reaching each other",
  broker: "Through a broker",
};

const groupOf = (key: string) => key.split(".")[0] ?? "";

/** The part after the first dot, as a sentence starts. */
function labelOf(key: string): string {
  const rest = key.slice(key.indexOf(".") + 1).replace(/_/g, " ");
  return rest.charAt(0).toUpperCase() + rest.slice(1);
}

/**
 * Whether the file can decide this one, and what to say where it cannot.
 *
 * Two reasons it cannot, and they are different things. Six settings may only
 * be changed at the machine: each either takes away the ability of whoever is
 * in front of the screen to know what is happening to it, or widens what a
 * paired device may reach without a grant. And a setting the *environment*
 * decides is one a change here would not reach — writing the file changes
 * nothing while `ESPER_HOST_FPS` is set, and a control that silently does
 * nothing is worse than none.
 */
function whyNotHere(setting: Setting, file: string | null): string | null {
  // Nowhere to write it down. A machine with no home directory runs entirely
  // on defaults, and a control that was offered would fail on every change.
  if (file === null) {
    return "There is nowhere to write this down: this machine has no home directory.";
  }
  if (setting.at_the_machine) {
    return setting.why_at_the_machine ?? "It is changed at the machine.";
  }
  if (/environment|command line|flag/i.test(setting.source)) {
    return `This comes from the ${setting.source}, which beats the file — a change here would not reach it.`;
  }
  return null;
}

export function EngineSettings() {
  const toast = useToast();
  const [answer, setAnswer] = useState<SettingsAnswer | null>(null);
  const [trouble, setTrouble] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const look = useCallback(() => {
    api
      .settings()
      .then((a) => {
        setAnswer(a);
        setTrouble(null);
      })
      .catch((cause: unknown) => setTrouble(cause instanceof Error ? cause.message : String(cause)));
  }, []);
  useEffect(look, [look]);

  /*
    One at a time, and the answer is read back.

    The engine answers a change with the whole registry as it now stands, so
    what is on screen afterwards is what the file says rather than what was
    asked for — which is the only way a change the environment shadows is
    visible.
  */
  const change = async (setting: Setting, raw: string) => {
    setBusy(setting.key);
    try {
      const after = await api.setSetting(setting.key, raw);
      setAnswer(after);
      const now = after.settings.find((s) => s.key === setting.key);
      toast.show({ title: `${setting.key} is ${now?.raw ?? raw}`, tone: "ok" });
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(null);
    }
  };

  const restore = async (setting: Setting) => {
    setBusy(setting.key);
    try {
      setAnswer(await api.unsetSetting(setting.key));
      toast.show({ title: `${setting.key} is back to what this build chooses`, tone: "ok" });
    } catch (cause) {
      toast.show({ title: cause instanceof Error ? cause.message : String(cause), tone: "warn" });
    } finally {
      setBusy(null);
    }
  };

  // Where each heading goes: at the first setting of each group, in the
  // engine's own order.
  const rows = useMemo(() => {
    let seen = "";
    return (answer?.settings ?? []).map((setting) => {
      const group = groupOf(setting.key);
      const heads = group !== seen;
      seen = group;
      return { setting, group, heads };
    });
  }, [answer]);

  if (trouble) {
    return <Trouble detail={trouble}>What this machine is set to could not be read.</Trouble>;
  }
  if (!answer) {
    return (
      <div className="flex flex-col gap-3 py-2">
        {[0, 1, 2, 3].map((i) => (
          <Skeleton key={i} className="h-3 w-full max-w-sm" />
        ))}
      </div>
    );
  }

  return (
    <>
      {/*
        Where the values live, said once. "Nothing written down" is the
        ordinary case on a machine nobody has configured, and a screen that
        showed a path without saying so sends somebody looking for a file that
        is not missing.
      */}
      <p data-config-file="" className="pb-2 text-xs leading-relaxed text-text-3">
        {answer.file === null
          ? "This machine has no home directory, so nothing can be written down."
          : answer.present
            ? `Written in ${answer.file}`
            : `Nothing is written down yet. The first change here creates ${answer.file}.`}
      </p>

      {rows.map(({ setting, group, heads }) => (
        <div key={setting.key} className="min-w-0">
          {heads && <Heading>{GROUP_NAMES[group] ?? group}</Heading>}
          <Row
            setting={setting}
            file={answer.file}
            busy={busy === setting.key}
            onChange={(raw) => void change(setting, raw)}
            onRestore={() => void restore(setting)}
          />
        </div>
      ))}
    </>
  );
}

function Row({
  setting,
  file,
  busy,
  onChange,
  onRestore,
}: {
  setting: Setting;
  file: string | null;
  busy: boolean;
  onChange: (raw: string) => void;
  onRestore: () => void;
}) {
  const why = whyNotHere(setting, file);
  // Whether this machine has been told something other than the default,
  // which is what makes "put it back" worth offering at all.
  const chosen = setting.raw !== String(setting.default ?? "none");

  return (
    <SettingRow
      className="min-w-0"
      label={labelOf(setting.key)}
      state={
        <span data-setting={setting.key} className="flex min-w-0 flex-col gap-0.5">
          <span>{setting.about}</span>
          <span className="font-mono">
            {setting.key} · {setting.source}
          </span>
        </span>
      }
      control={
        <span className="flex items-center gap-2">
          {!why && chosen && (
            <Button variant="ghost" size="sm" disabled={busy} onClick={onRestore} className="text-text-3">
              Put it back
            </Button>
          )}
          <Control setting={setting} disabled={busy || why !== null} onChange={onChange} />
        </span>
      }
      trouble={
        why ? (
          <p data-not-here="" className="text-xs leading-relaxed text-text-3">
            {why}
            {setting.at_the_machine && (
              <span className="ml-1 font-mono text-text-2">esper config set {setting.key} …</span>
            )}
          </p>
        ) : undefined
      }
    />
  );
}

/** The control this setting's kind wants. */
function Control({
  setting,
  disabled,
  onChange,
}: {
  setting: Setting;
  disabled: boolean;
  onChange: (raw: string) => void;
}) {
  const named = `${labelOf(setting.key)} (${setting.key})`;

  switch (setting.kind) {
    case "flag":
      return (
        <Switch
          checked={setting.value === true}
          disabled={disabled}
          onCheckedChange={(on) => onChange(on ? "true" : "false")}
          aria-label={named}
        />
      );

    case "choice":
      return (
        <Select value={setting.raw} disabled={disabled} onValueChange={onChange}>
          <SelectTrigger className="w-44" aria-label={named}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(setting.choices ?? []).map((word) => (
              <SelectItem key={word} value={word}>
                {word}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      );

    case "time":
      // Always seconds underneath; the unit is only how it is written. A
      // retention of 7,776,000 is ninety days, and nobody reads the first.
      return (
        <DurationInput
          value={typeof setting.value === "number" ? setting.value : 0}
          disabled={disabled}
          onChange={(seconds) => onChange(String(seconds))}
          aria-label={named}
          className="w-52"
        />
      );

    case "whole_or_none":
      /*
        A number, or the word that means "decide for me".

        `host.max_resolution` is the case: none sends the screen's own size,
        and it is a real choice somebody makes — a number box alone could
        never express it, so the screen would be able to show a value the file
        holds and not set it.
      */
      return (
        <span className="flex items-center gap-2">
          {setting.value !== null && (
            <SavedNumber setting={setting} disabled={disabled} onChange={onChange} named={named} />
          )}
          <label className="flex items-center gap-1.5 text-xs text-text-3">
            Automatic
            <Switch
              checked={setting.value === null}
              disabled={disabled}
              onCheckedChange={(on) => onChange(on ? "none" : String(setting.least ?? 1))}
              aria-label={`${named}: automatic`}
            />
          </label>
        </span>
      );

    case "whole":
    case "cores":
      return <SavedNumber setting={setting} disabled={disabled} onChange={onChange} named={named} />;

    default:
      return <SavedText setting={setting} disabled={disabled} onChange={onChange} named={named} />;
  }
}

/**
 * A number that is sent when somebody has finished typing it.
 *
 * Not on every keystroke: `240` is typed as `2`, then `24`, and both are
 * values the file would take and the machine would then be running on. Sent
 * on blur and on Enter, which is when somebody means it.
 */
function SavedNumber({
  setting,
  disabled,
  onChange,
  named,
}: {
  setting: Setting;
  disabled: boolean;
  onChange: (raw: string) => void;
  named: string;
}) {
  const asNumber = typeof setting.value === "number" ? setting.value : "";
  const [draft, setDraft] = useState<number | "">(asNumber);
  // Follows the answer when it changes underneath, which it does after every
  // saved change and after a "put it back".
  useEffect(() => setDraft(asNumber), [asNumber]);

  const send = () => {
    if (draft === "" || draft === asNumber) return;
    onChange(String(draft));
  };

  return (
    <NumberInput
      value={draft}
      disabled={disabled}
      min={setting.least}
      max={setting.most}
      step={setting.kind === "cores" ? 0.5 : 1}
      onChange={setDraft}
      onBlur={send}
      onKeyDown={(e) => {
        if (e.key === "Enter") send();
      }}
      aria-label={named}
      className="w-32"
    />
  );
}

/** The same, for text: sent on blur and on Enter rather than as it is typed. */
function SavedText({
  setting,
  disabled,
  onChange,
  named,
}: {
  setting: Setting;
  disabled: boolean;
  onChange: (raw: string) => void;
  named: string;
}) {
  const [draft, setDraft] = useState(setting.raw);
  useEffect(() => setDraft(setting.raw), [setting.raw]);

  const send = () => {
    if (draft === setting.raw) return;
    onChange(draft);
  };

  return (
    <Input
      value={draft}
      disabled={disabled}
      placeholder={setting.shape}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={send}
      onKeyDown={(e) => {
        if (e.key === "Enter") send();
      }}
      aria-label={named}
      className="w-56"
    />
  );
}
