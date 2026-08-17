import type { ReactNode } from "react";
import type { MentionCandidate } from "./mentionSuggestions";
import { noiseSignature } from "./noiseSignature";

export function MentionPicker({
  people,
  selectedIndex,
  onSelect,
  onHover,
  renderAvatar,
}: {
  people: MentionCandidate[];
  selectedIndex: number;
  onSelect: (person: MentionCandidate) => void;
  onHover: (index: number) => void;
  renderAvatar: (person: MentionCandidate) => ReactNode;
}) {
  if (people.length === 0) return null;
  return (
    <div className="mention-picker" role="listbox" aria-label="mention someone">
      {people.map((person, index) => {
        const signature = noiseSignature(person.public_key);
        return (
          <button
            key={person.public_key}
            type="button"
            role="option"
            aria-selected={index === selectedIndex}
            className={index === selectedIndex ? "selected" : undefined}
            onMouseEnter={() => onHover(index)}
            onMouseDown={(event) => {
              event.preventDefault();
              onSelect(person);
            }}
          >
            {renderAvatar(person)}
            <span className="mention-picker-copy">
              <strong>{person.username}</strong>
              {signature !== "UNAVAILABLE" && <small>{signature}</small>}
            </span>
          </button>
        );
      })}
    </div>
  );
}
