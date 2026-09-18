import { useRef, useState, type CSSProperties } from "react";

import { openSession } from "../actions";
import {
  encodingLabel,
  DEFAULT_ENCODING,
  LOCALE_NAME,
  LOCALE_SUGGESTIONS,
  TERMINAL_ENCODINGS,
} from "../encodings";
import { IS_WINDOWS } from "../platform";
import {
  describeLocation,
  flattenGroups,
  groupPath,
} from "../sessionGroups";
import { useStore } from "../store";
import {
  endDialogAttention,
  requestDialogAttention,
} from "./dialogAttention";
import { GroupNameDialog } from "./GroupNameDialog";
import { useDialogDrag } from "./useDialogDrag";
import {
  colorForSession,
  isFileSession,
  isSshTransport,
  randomSessionColor,
  SESSION_COLORS,
  type SessionKind,
  type SessionProfile,
} from "../types";
import { Icon, type IconName } from "./icons";

/**
 * Sentinel for the Group field's "New group…" choice; group ids are uuids, so
 * it cannot collide with a real one.
 */
const NEW_GROUP = "__new__";

interface Props {
  initial: SessionProfile | null;
  onClose: () => void;
}

const BLANK: SessionProfile = {
  id: "",
  name: "",
  kind: "ssh",
  port: 22,
  auth: "password",
};

const RAW_TEXT_INPUT = {
  autoCapitalize: "none",
  autoCorrect: "off",
  spellCheck: false,
} as const;

const defaultPort = (kind: SessionKind, current?: number | null) =>
  kind === "ssh" || kind === "sftp" ? (current ?? 22) : current;

/**
 * Protocol picker entries. `kinds[0]` is the kind the choice selects; the
 * array form is kept because `protocolIcon` looks a kind up through it.
 */
const PROTOCOL_OPTIONS: {
  label: string;
  icon: IconName;
  kinds: SessionKind[];
}[] = [
  { label: "SSH", icon: "server", kinds: ["ssh"] },
  { label: "SFTP", icon: "folder", kinds: ["sftp"] },
];

/** The picker icon of the choice a kind belongs to; also the dialog's badge. */
const protocolIcon = (kind: SessionKind): IconName =>
  PROTOCOL_OPTIONS.find((option) => option.kinds.includes(kind))?.icon ??
  "server";

/** Column count of `.session-color-picker`; keep in sync with styles.css. */
const COLOR_PICKER_COLUMNS = 8;

/**
 * Saved SSH transports `profile` may be tunnelled through: every SSH / SFTP
 * profile except itself and those whose own jump chain already leads back to
 * it, which would loop. Mirrors the cycle check in the backend's
 * `Store::save`, so the menu never offers a choice that save would refuse.
 */
function jumpHostChoices(
  profile: SessionProfile,
  profiles: SessionProfile[],
): SessionProfile[] {
  const byId = new Map(profiles.map((p) => [p.id, p]));
  const leadsBackHere = (candidate: SessionProfile): boolean => {
    const seen = new Set<string>();
    let current: SessionProfile | undefined = candidate;
    while (current) {
      if (current.id === profile.id) return true;
      if (seen.has(current.id)) return false;
      seen.add(current.id);
      current = current.jumpProfileId
        ? byId.get(current.jumpProfileId)
        : undefined;
    }
    return false;
  };
  return profiles.filter(
    (p) =>
      p.id &&
      p.id !== profile.id &&
      isSshTransport(p.kind) &&
      !(profile.id && leadsBackHere(p)),
  );
}

const describeJumpHost = (p: SessionProfile) =>
  `${p.name} — ${p.username ?? ""}@${p.host ?? ""}:${p.port ?? 22}`;

function validateProfile(profile: SessionProfile): string | null {
  if (profile.locale && !LOCALE_NAME.test(profile.locale)) {
    return "Locale must be a locale name such as en_US.UTF-8 or C.UTF-8.";
  }
  return null;
}

function nextColorIndex(
  key: string,
  current: number,
  count: number,
  columns: number,
): number | null {
  switch (key) {
    case "ArrowRight":
      return (current + 1) % count;
    case "ArrowLeft":
      return (current - 1 + count) % count;
    case "ArrowDown":
      return current + columns < count ? current + columns : current % columns;
    case "ArrowUp": {
      if (current - columns >= 0) return current - columns;
      const lastRowStart = Math.floor((count - 1) / columns) * columns;
      const target = lastRowStart + (current % columns);
      return target < count ? target : target - columns;
    }
    case "Home":
      return 0;
    case "End":
      return count - 1;
    default:
      return null;
  }
}

export function SessionDialog({ initial, onClose }: Props) {
  const colorPickerRef = useRef<HTMLDivElement>(null);
  const { dialogRef, handleProps: dragHandleProps } =
    useDialogDrag<HTMLDivElement>();
  const [profile, setProfile] = useState<SessionProfile>(() =>
    initial
      ? {
          ...initial,
          color:
            initial.color ?? colorForSession(initial.id || initial.name),
        }
      : { ...BLANK, color: randomSessionColor() },
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The Group field's "New group…" prompt, drawn over this dialog.
  const [creatingGroup, setCreatingGroup] = useState(false);

  const upsertProfile = useStore((s) => s.upsertProfile);
  const upsertGroup = useStore((s) => s.upsertGroup);
  const groups = useStore((s) => s.groups);
  const profiles = useStore((s) => s.profiles);
  const groupChoices = flattenGroups(groups);
  const jumpChoices = jumpHostChoices(profile, profiles);
  // The chosen jump session was deleted (or now loops back here): keep it
  // visible so the user sees what is wrong; saving drops it.
  const jumpHostMissing =
    !!profile.jumpProfileId &&
    !jumpChoices.some((p) => p.id === profile.jumpProfileId);

  const patch = (fields: Partial<SessionProfile>) =>
    setProfile((prev) => ({ ...prev, ...fields }));

  const defaultName = () => {
    if (profile.kind === "ssh") return profile.host ?? "ssh";
    if (profile.kind === "sftp") return profile.host ?? "sftp";
    return "shell";
  };

  const normalized = (): SessionProfile => ({
    ...profile,
    name: profile.name.trim() || defaultName(),
    // A jump host only means something on an SSH transport; drop one left
    // over from before the protocol was switched.
    jumpProfileId: isSshTransport(profile.kind)
      ? profile.jumpProfileId || null
      : null,
    // Encoding and locale belong to terminal sessions; UTF-8 and "automatic"
    // are stored as nothing, so an untouched profile stays as it was.
    encoding:
      !isFileSession(profile.kind) &&
      encodingLabel(profile.encoding) !== DEFAULT_ENCODING
        ? encodingLabel(profile.encoding)
        : null,
    locale:
      profile.kind === "local" || profile.kind === "ssh"
        ? profile.locale?.trim() || null
        : null,
  });

  // What the session's bytes mean, for every terminal kind, and the locale
  // to ask the shell for — a local shell gets it as LANG, an SSH server is
  // asked for it.
  const renderTextFields = (withLocale: boolean) => {
    const encoding = encodingLabel(profile.encoding);
    const choices = TERMINAL_ENCODINGS.some((e) => e.label === encoding)
      ? TERMINAL_ENCODINGS
      : [...TERMINAL_ENCODINGS, { label: encoding, name: encoding }];
    return (
      <>
        <label className="session-field">
          <span className="session-field-label">Encoding</span>
          <select
            value={encoding}
            onChange={(event) => patch({ encoding: event.target.value })}
          >
            {choices.map((choice) => (
              <option key={choice.label} value={choice.label}>
                {choice.name}
              </option>
            ))}
          </select>
        </label>
        {withLocale && (
          <>
            <label className="session-field">
              <span className="session-field-label">Locale</span>
              <input
                {...RAW_TEXT_INPUT}
                list="session-locale-suggestions"
                value={profile.locale ?? ""}
                placeholder="Automatic"
                onChange={(event) => patch({ locale: event.target.value })}
              />
              <datalist id="session-locale-suggestions">
                {LOCALE_SUGGESTIONS.map((locale) => (
                  <option key={locale} value={locale} />
                ))}
              </datalist>
            </label>
            <div className="session-note is-wide">
              <Icon name="info" />
              <span>
                {profile.kind === "ssh"
                  ? "The locale is sent as LANG when the shell starts, so " +
                    "a server with AcceptEnv LANG prints file names in " +
                    "UTF-8 instead of octal escapes. Empty keeps the " +
                    "server's default."
                  : "The locale becomes the shell's LANG. Empty inherits " +
                    "the environment, with a UTF-8 locale filled in when " +
                    "the environment names none."}
              </span>
            </div>
          </>
        )}
      </>
    );
  };

  // ProxyJump: tunnel this session through another saved SSH session.
  // Offered for SSH and SFTP alike, since both ride the same transport.
  const renderJumpHostField = () => (
    <>
      <label className="session-field is-wide">
        <span className="session-field-label">Jump host</span>
        <select
          value={profile.jumpProfileId ?? ""}
          onChange={(event) =>
            patch({ jumpProfileId: event.target.value || null })
          }
        >
          <option value="">None — connect directly</option>
          {jumpHostMissing && (
            <option value={profile.jumpProfileId ?? ""}>
              (deleted session)
            </option>
          )}
          {jumpChoices.map((p) => (
            <option key={p.id} value={p.id}>
              {describeJumpHost(p)}
            </option>
          ))}
        </select>
      </label>
      <div className="session-note is-wide">
        <Icon name="info" />
        <span>
          Connect through a saved SSH session (ProxyJump) to reach a host that
          is only visible from its network.
        </span>
      </div>
    </>
  );

  // The authentication block shared by SSH and SFTP: they ride the same
  // transport, so both offer password, public-key, and ssh-agent auth with the
  // exact same inputs.
  const renderServerAuthFields = () => (
    <>
      <label className="session-field">
        <span className="session-field-label">Authentication</span>
        <select
          value={profile.auth ?? "password"}
          onChange={(event) =>
            patch({ auth: event.target.value as SessionProfile["auth"] })
          }
        >
          <option value="password">Password</option>
          <option value="publicKey">Public key</option>
          <option value="agent">SSH agent</option>
        </select>
      </label>

      {profile.auth === "password" && (
        <label className="session-field is-wide">
          <span className="session-field-label">Password</span>
          <input
            {...RAW_TEXT_INPUT}
            type="password"
            value={profile.password ?? ""}
            onChange={(event) => patch({ password: event.target.value })}
          />
        </label>
      )}

      {profile.auth === "publicKey" && (
        <>
          <label className="session-field is-wide">
            <span className="session-field-label">Private key</span>
            <input
              {...RAW_TEXT_INPUT}
              value={profile.privateKeyPath ?? ""}
              placeholder="~/.ssh/id_ed25519"
              onChange={(event) => patch({ privateKeyPath: event.target.value })}
            />
          </label>
          <label className="session-field is-wide">
            <span className="session-field-label">Passphrase</span>
            <input
              {...RAW_TEXT_INPUT}
              type="password"
              value={profile.passphrase ?? ""}
              onChange={(event) => patch({ passphrase: event.target.value })}
            />
          </label>
        </>
      )}
    </>
  );

  const save = async () => {
    const candidate = normalized();
    const validationError = validateProfile(candidate);
    if (validationError) {
      setError(validationError);
      return;
    }

    setBusy(true);
    setError(null);
    try {
      await upsertProfile(candidate);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const connect = async () => {
    const candidate = normalized();
    const validationError = validateProfile(candidate);
    if (validationError) {
      setError(validationError);
      return;
    }

    setBusy(true);
    setError(null);
    try {
      // Saving first keeps the profile in the tree and stores its credentials
      // in the operating system's secure credential vault.
      const saved = candidate.name ? await upsertProfile(candidate) : candidate;
      const id = await openSession({
        ...saved,
        password: candidate.password,
        passphrase: candidate.passphrase,
      });
      if (id) onClose();
      else setError(useStore.getState().error ?? "connection failed");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(event) => {
        // A stray click outside must not discard the form (issue #33): keep
        // the dialog and its focus, and flash it so the click is answered.
        event.preventDefault();
        requestDialogAttention(dialogRef.current);
      }}
    >
      <div
        ref={dialogRef}
        className="dialog session-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="session-dialog-title"
        onMouseDown={(event) => event.stopPropagation()}
        onAnimationEnd={endDialogAttention}
      >
        <div
          className="dialog-header session-dialog-header is-drag-handle"
          {...dragHandleProps}
        >
          {/* The badge carries the session colour picked below, so the
              dialog wears the identity the tab will have. */}
          <span
            className="session-dialog-badge"
            style={
              {
                "--session-color": profile.color ?? "var(--accent)",
              } as CSSProperties
            }
            aria-hidden="true"
          >
            <Icon name={protocolIcon(profile.kind)} />
          </span>
          <div className="session-dialog-heading">
            <div id="session-dialog-title" className="session-dialog-title">
              {initial?.id ? "Edit Session" : "New Session"}
            </div>
          </div>
          <button
            className="panel-action"
            type="button"
            onClick={onClose}
            aria-label="Close"
          >
            <Icon name="close" />
          </button>
        </div>

        <div className="dialog-body session-dialog-body">
          <section className="session-section">
            <div className="session-section-heading">
              <Icon name="tag" />
              <span>Session</span>
            </div>
            <div className="session-form-grid">
              <div className="session-field is-wide">
                <span className="session-field-label">Protocol</span>
                <div
                  className="protocol-picker"
                  role="radiogroup"
                  aria-label="Protocol"
                >
                  {PROTOCOL_OPTIONS.map((option) => {
                    const active = option.kinds.includes(profile.kind);
                    // Keep the current sub-choice when the group already
                    // matches; otherwise select the option's default kind.
                    const target = active ? profile.kind : option.kinds[0];
                    return (
                      <button
                        key={option.label}
                        type="button"
                        role="radio"
                        aria-checked={active}
                        className={`protocol-option${active ? " is-active" : ""}`}
                        onClick={() =>
                          patch({
                            kind: target,
                            port: defaultPort(
                              target,
                              profile.kind === target ? profile.port : null,
                            ),
                            // A group belongs to one kind, so switching the
                            // protocol clears a choice the new kind's section
                            // would not show.
                            groupId:
                              profile.kind === target ? profile.groupId : null,
                          })
                        }
                      >
                        <span className="protocol-option-icon">
                          <Icon name={option.icon} />
                        </span>
                        <span className="protocol-option-label">
                          {option.label}
                        </span>
                      </button>
                    );
                  })}
                </div>
              </div>

              <label className="session-field">
                <span className="session-field-label">Name</span>
                <input
                  {...RAW_TEXT_INPUT}
                  value={profile.name}
                  placeholder={defaultName()}
                  onChange={(event) => patch({ name: event.target.value })}
                />
              </label>

              <div className="session-field">
                <span className="session-field-label">Color</span>
                <div
                  ref={colorPickerRef}
                  className="session-color-picker"
                  role="radiogroup"
                  aria-label="Session color"
                  onKeyDown={(event) => {
                    const current = SESSION_COLORS.indexOf(
                      (event.target as HTMLElement).dataset.color ?? "",
                    );
                    if (current < 0) return;
                    const next = nextColorIndex(
                      event.key,
                      current,
                      SESSION_COLORS.length,
                      COLOR_PICKER_COLUMNS,
                    );
                    if (next == null) return;
                    event.preventDefault();
                    const color = SESSION_COLORS[next];
                    patch({ color });
                    colorPickerRef.current
                      ?.querySelector<HTMLButtonElement>(
                        `[data-color="${color}"]`,
                      )
                      ?.focus();
                  }}
                >
                  {SESSION_COLORS.map((color, index) => {
                    const selected = profile.color === color;
                    // Roving tabindex: only the selected swatch is a Tab stop,
                    // so Tab moves on to the next field and arrow keys pick colors.
                    const tabbable =
                      selected ||
                      (!SESSION_COLORS.includes(profile.color ?? "") &&
                        index === 0);
                    return (
                      <button
                        key={color}
                        type="button"
                        role="radio"
                        data-color={color}
                        tabIndex={tabbable ? 0 : -1}
                        className={`session-color-option${selected ? " is-selected" : ""}`}
                        style={{ background: color }}
                        aria-checked={selected}
                        aria-label={`Use session color ${color}`}
                        title={color}
                        onClick={() => patch({ color })}
                      />
                    );
                  })}
                </div>
              </div>
              <div className="session-field is-wide">
                <label className="session-field-label" htmlFor="session-group">
                  Group
                </label>
                <select
                  id="session-group"
                  value={profile.groupId ?? ""}
                  onChange={(event) => {
                    // A group is made where it is used. Picking this opens the
                    // name prompt and selects whatever it creates; the select
                    // snaps back meanwhile, since `profile.groupId` is
                    // untouched until the group exists.
                    if (event.target.value === NEW_GROUP) {
                      setCreatingGroup(true);
                      return;
                    }
                    patch({ groupId: event.target.value || null });
                  }}
                >
                  <option value="">Top level (no group)</option>
                  {groupChoices.map(({ group }) => (
                    <option key={group.id} value={group.id}>
                      {groupPath(groups, group.id).join(" / ")}
                    </option>
                  ))}
                  <option value={NEW_GROUP}>New group…</option>
                </select>
              </div>
            </div>
          </section>

          {profile.kind === "ssh" && (
            <section className="session-section">
              <div className="session-section-heading">
                <Icon name="server" />
                <span>SSH connection</span>
              </div>
              <div className="session-form-grid">
                <label className="session-field">
                  <span className="session-field-label">Host</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.host ?? ""}
                    placeholder="example.com"
                    onChange={(event) => patch({ host: event.target.value })}
                  />
                </label>

                <label className="session-field">
                  <span className="session-field-label">Port</span>
                  <input
                    type="number"
                    min={1}
                    max={65535}
                    value={profile.port ?? 22}
                    onChange={(event) =>
                      patch({ port: Number(event.target.value) || 22 })
                    }
                  />
                </label>

                <label className="session-field">
                  <span className="session-field-label">Username</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.username ?? ""}
                    placeholder="user"
                    onChange={(event) => patch({ username: event.target.value })}
                  />
                </label>

                {renderServerAuthFields()}
                {renderJumpHostField()}
                {renderTextFields(true)}
              </div>
            </section>
          )}

          {profile.kind === "sftp" && (
            <section className="session-section">
              <div className="session-section-heading">
                <Icon name="folder" />
                <span>SFTP connection</span>
              </div>
              <div className="session-form-grid">
                <label className="session-field">
                  <span className="session-field-label">Host</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.host ?? ""}
                    placeholder="sftp.example.com"
                    onChange={(event) => patch({ host: event.target.value })}
                  />
                </label>

                <label className="session-field">
                  <span className="session-field-label">Port</span>
                  <input
                    type="number"
                    min={1}
                    max={65535}
                    value={profile.port ?? 22}
                    onChange={(event) =>
                      patch({ port: Number(event.target.value) || 22 })
                    }
                  />
                </label>

                <label className="session-field">
                  <span className="session-field-label">Username</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.username ?? ""}
                    placeholder="user"
                    onChange={(event) => patch({ username: event.target.value })}
                  />
                </label>

                {renderServerAuthFields()}
                {renderJumpHostField()}
              </div>
            </section>
          )}

          {profile.kind === "local" && (
            <section className="session-section">
              <div className="session-section-heading">
                <Icon name="terminal" />
                <span>Local shell</span>
              </div>
              <div className="session-form-grid">
                <label className="session-field is-wide">
                  <span className="session-field-label">Shell</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.shell ?? ""}
                    placeholder={IS_WINDOWS ? "%COMSPEC%" : "$SHELL"}
                    onChange={(event) => patch({ shell: event.target.value })}
                  />
                </label>
                <label className="session-field is-wide">
                  <span className="session-field-label">Directory</span>
                  <input
                    {...RAW_TEXT_INPUT}
                    value={profile.cwd ?? ""}
                    placeholder="Home directory"
                    onChange={(event) => patch({ cwd: event.target.value })}
                  />
                </label>
                {renderTextFields(true)}
              </div>
            </section>
          )}

          {error && (
            <div className="dialog-error session-dialog-error" role="alert">
              <Icon name="error" />
              <span>{error}</span>
            </div>
          )}
        </div>

        <div className="dialog-footer">
          <button type="button" className="btn" onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="btn" onClick={save} disabled={busy}>
            <Icon name="save" />
            Save
          </button>
          <button
            type="button"
            className="btn is-primary"
            onClick={connect}
            disabled={busy}
          >
            <Icon name="plug" />
            {busy ? "Connecting…" : "Connect"}
          </button>
        </div>
      </div>

      {creatingGroup && (
        <GroupNameDialog
          title="New Group"
          location={describeLocation(groups, null)}
          submitLabel="Create"
          onSubmit={async (name) => {
            const saved = await upsertGroup({ id: "", name, parentId: null });
            patch({ groupId: saved.id });
            setCreatingGroup(false);
          }}
          onCancel={() => setCreatingGroup(false)}
        />
      )}
    </div>
  );
}
