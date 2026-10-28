import { Alarm } from "@/components/alarm";
import { Button } from "@/components/button";
import { Callout } from "@/components/callout";
import { Lamp } from "@/components/lamp";
import { Said, SettingRow } from "@/components/setting";
import { Switch } from "@/components/switch";
import { Figure, Fingerprint, Heading, Key, Row, Trouble } from "@/screens/parts";
import { Demo, Section, Sub, TokenName } from "./ui";

const readings = [
  { label: "Round trip", value: "18", unit: "ms" },
  { label: "Bitrate", value: "2.4", unit: "Mbit/s" },
  { label: "Frames", value: "60", unit: "fps" },
  { label: "Dropped", value: "0", unit: "" },
];

/**
 * The vocabulary the product screens are actually built from — which was not
 * on this page.
 *
 * Every screen in Esper is made of seven things: a key, a figure, a row, a
 * fact, a fingerprint, a heading and a trouble. They are the most-used set in
 * the product by a wide margin and the gate rendered none of them, so nothing
 * showed them in three themes and nothing checked they stayed one family. A
 * design system that documents its buttons and not its rows is documenting
 * the part nobody argues about.
 */
export function VocabularySection() {
  return (
    <Section
      id="vocabulary"
      title="The instrument panel"
      lede="Seven small parts every screen is built from. Labels are mono caps, numbers are aligned, rows are lines rather than boxes."
    >
      <div className="grid gap-8 lg:grid-cols-2">
        <Sub
          title="Key and figure"
          about="A label and a value, with numbers aligned so a column can be scanned."
        >
          <Demo className="flex-col items-stretch gap-0 p-0">
            <div className="px-5">
              <Heading count={readings.length}>Link</Heading>
            </div>
            <div className="px-5 pb-2">
              {readings.map((r) => (
                <Row key={r.label} className="items-baseline last:border-0">
                  <span className="w-28 shrink-0">
                    <Key>{r.label}</Key>
                  </span>
                  <Figure value={r.value} unit={r.unit} className="text-sm" />
                </Row>
              ))}
            </div>
          </Demo>
        </Sub>

        <Sub title="Fingerprint" about="Grouped in fours, the way it is read aloud.">
          <Demo className="flex-col items-stretch gap-1.5">
            <Key>Key</Key>
            <Fingerprint hex="a3f19c0428be71d5" />
          </Demo>
        </Sub>

        <Sub
          title="Lamp"
          about="A status dot that always comes with its word."
        >
          <Demo className="gap-6 text-sm text-text">
            <Lamp state="ok">available</Lamp>
            <Lamp state="warn">no portal</Lamp>
            <Lamp state="off">not granted</Lamp>
          </Demo>
        </Sub>

        <Sub
          title="A setting is a label, a state and one control"
          about="One setting per line: a label, what it does, and its control on the right."
        >
          <Demo className="flex-col items-stretch gap-0 px-5 py-1">
            <SettingRow
              label="Serving"
              state="Running now · starts at login · systemd"
              control={<Switch defaultChecked aria-label="Share this machine's screen" />}
            />
            <SettingRow
              label="At login"
              state={<span className="block truncate font-mono">~/.config/autostart/esper.desktop</span>}
              control={<Switch aria-label="Open Esper when I log in" />}
              className="border-0"
            />
          </Demo>
        </Sub>
      </div>

      <Sub
        title="Verbatim, when the machine did something on its own"
        about="The commands a switch runs, shown so a half-finished setup can be finished by hand."
      >
        <Demo className="flex-col items-stretch">
          <Said>
            {[
              "systemctl --user enable --now esper-host.service",
              "loginctl enable-linger elythion",
              "systemctl --user is-active esper-host.service → active",
            ].join("\n")}
          </Said>
        </Demo>
      </Sub>

      <Sub
        title="Trouble, callout or alarm"
        about="Three ways to say something is wrong, chosen by whose problem it is."
      >
        <div className="grid gap-6 lg:grid-cols-3">
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Trouble</span>: this request failed. It sits where the answer would
              have been and says what to do. The API's own message goes underneath in mono, because it names
              the file or address.
            </p>
            <Demo className="flex-col items-stretch">
              <Trouble detail="dial tcp 127.0.0.1:8687: connect: connection refused" action={<Button size="sm">Try again</Button>}>
                Could not reach Esper on this machine, so its features can't be checked.
              </Trouble>
            </Demo>
          </div>
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Callout</span>: an ongoing condition of the screen you are on. It is
              not a failure and not urgent. It has an icon, a title and a sentence, in a tone.
            </p>
            <Demo className="flex-col items-stretch">
              <Callout tone="warn" title="This device can control this machine">
                It can use the mouse and keyboard as well as see the screen. Turn off control to let it only watch.
              </Callout>
            </Demo>
          </div>
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Alarm</span>: a problem with the machine, true on every screen. It is
              a band across the whole window, sidebar included, and it can't be dismissed until the problem is
              fixed.
            </p>
            {/* In a shell frame, because the band's compact arrangement is a
                container query on one — a bare specimen would be showing an
                arrangement the product never uses. */}
            <Demo className="flex-col items-stretch p-0">
              <div className="shell-frame">
              <Alarm
                show
                detail="signal: killed"
                action={<Button size="sm" variant="primary">Restart Esper</Button>}
              >
                Esper's background service on this machine has stopped.
              </Alarm>
              </div>
            </Demo>
          </div>
        </div>
        <p className="max-w-[var(--measure)] text-sm text-text-3">
          None of the three is a toast. A toast reports what just happened after someone pressed something.
          These are states that stay true when nobody is looking. <TokenName>Alarm</TokenName>{" "}
          is the only one that outranks the screen it is on.
        </p>
      </Sub>
    </Section>
  );
}
