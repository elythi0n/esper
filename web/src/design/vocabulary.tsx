import { Alarm } from "@/components/alarm";
import { Button } from "@/components/button";
import { Callout } from "@/components/callout";
import { Lamp } from "@/components/lamp";
import { Said, SettingRow } from "@/components/setting";
import { Switch } from "@/components/switch";
import { Fact, Figure, Fingerprint, Heading, Key, Row, Trouble } from "@/screens/parts";
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
      lede="Seven small things every product screen is built from, and the three rules behind them: a key is mono, uppercase and tracked; a figure is mono, tabular and never animates; a row is a hairline, not a box. Boxes around everything is how a dense screen becomes a scrapbook."
    >
      <div className="grid gap-8 lg:grid-cols-2">
        <Sub
          title="Key and figure"
          about="A field name and a value. The figure is tabular so a column of them lines up under itself — the whole reason a readout can be scanned instead of read — and it carries its unit in the muted tone, at the size of the number rather than a size of its own."
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

        <Sub
          title="Fact and fingerprint"
          about="A key above its value, for a panel rather than a list. The fingerprint is set in groups of four because it is the one string in this product two people read aloud to each other, and it never breaks mid-group."
        >
          <Demo className="flex-col items-stretch gap-5">
            <div className="grid grid-cols-2 gap-5">
              <Fact label="Name">workstation</Fact>
              <Fact label="Paired">4 days ago</Fact>
              <Fact label="Tier">Local network</Fact>
              <Fact label="Permissions">screen, input, clipboard</Fact>
            </div>
            <div className="flex flex-col gap-1.5">
              <Key>Key</Key>
              <Fingerprint hex="a3f19c0428be71d5" />
            </div>
          </Demo>
        </Sub>

        <Sub
          title="Lamp"
          about="A dot and the word it is about, in one component — because “no colour-only meaning” was a rule nothing enforced, and the same six-pixel dot had been written out five times. The word is a required prop: a lamp on its own does not typecheck. Off is a hollow ring rather than a grey fill, since at this size a muted fill and a warn fill are the same dot to anybody not looking for the difference."
        >
          <Demo className="gap-6 text-sm text-text">
            <Lamp state="ok">available</Lamp>
            <Lamp state="warn">no portal</Lamp>
            <Lamp state="off">not granted</Lamp>
          </Demo>
        </Sub>

        <Sub
          title="A setting is a label, a state and one control"
          about="Written three times on the Settings screen before it was a component, and already drifted three ways. The label holds a fixed width and the control stays at the right edge, so a column of settings has one left edge and one right edge whatever length the sentences between them are. The state line is words the control cannot say: “running now, does not start at login, systemd” is three facts and no switch shows any of them."
        >
          <Demo className="flex-col items-stretch gap-0 px-5 py-1">
            <SettingRow
              label="Serving"
              state="Running now · starts at login · systemd"
              control={<Switch defaultChecked aria-label="Serve this machine's screen" />}
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
        about="Turning the host on runs four commands, and one of them is the one everybody misses. Showing them is what keeps a switch from being a black box: a machine where it half-worked is one somebody can finish by hand. It wraps rather than scrolling sideways, because the line with the long path in it is exactly the line somebody needs to copy."
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
        title="Trouble, callout, alarm — which one"
        about="Three ways to say something is wrong, and picking the wrong one is how a screen either shouts or hides. The rule is whose fact it is."
      >
        <div className="grid gap-6 lg:grid-cols-3">
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Trouble</span> — this request failed. It belongs where the answer
              would have been, says what to do, and puts the API's own sentence underneath in mono because
              that is the part naming the file or the address.
            </p>
            <Demo className="flex-col items-stretch">
              <Trouble detail="dial tcp 127.0.0.1:8687: connect: connection refused" action={<Button size="sm">Try again</Button>}>
                The engine did not answer, so what this machine can do is unknown.
              </Trouble>
            </Demo>
          </div>
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Callout</span> — a standing condition of the screen you are on.
              Not a failure and not urgent: an icon, a title and a sentence, in a tone.
            </p>
            <Demo className="flex-col items-stretch">
              <Callout tone="warn" title="This device may control this machine">
                It holds input as well as screen. Revoke input to make it watch only.
              </Callout>
            </Demo>
          </div>
          <div className="flex flex-col gap-3">
            <p className="max-w-[var(--measure)] text-sm text-text-2">
              <span className="text-text">Alarm</span> — a condition of the machine, true on every screen at
              once. A band across the whole window, over the sidebar too, and not dismissible: a band
              somebody can close stops being a fact and becomes a notification.
            </p>
            {/* In a shell frame, because the band's compact arrangement is a
                container query on one — a bare specimen would be showing an
                arrangement the product never uses. */}
            <Demo className="flex-col items-stretch p-0">
              <div className="shell-frame">
              <Alarm
                show
                detail="signal: killed"
                action={<Button size="sm" variant="primary">Start it again</Button>}
              >
                The engine on this machine has stopped.
              </Alarm>
              </div>
            </Demo>
          </div>
        </div>
        <p className="max-w-[var(--measure)] text-sm text-text-3">
          And none of the three is a toast. A toast is what just happened because somebody pressed
          something; these are all states that are still true when nobody is looking. <TokenName>Alarm</TokenName>{" "}
          is the only one that outranks the screen it is on.
        </p>
      </Sub>
    </Section>
  );
}
