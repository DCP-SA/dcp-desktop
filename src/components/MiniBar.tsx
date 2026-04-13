interface MiniBarProps {
  value: number;
  max: number;
  label: string;
  color: string;
  displayValue?: string;
}

export function MiniBar({
  value,
  max,
  label,
  color,
  displayValue,
}: MiniBarProps) {
  const percentage = max > 0 ? Math.min((value / max) * 100, 100) : 0;
  const display = displayValue ?? `$${value.toFixed(2)}`;

  return (
    <div className="minibar" role="meter" aria-valuenow={value} aria-valuemin={0} aria-valuemax={max} aria-label={label}>
      <div className="minibar-header">
        <span className="minibar-label">{label}</span>
        <span className="minibar-value" style={{ color }}>
          {display}
        </span>
      </div>
      <div className="minibar-track">
        <div
          className="minibar-fill"
          style={{
            width: `${percentage}%`,
            background: color,
            boxShadow: `0 0 8px ${color}40`,
            transition: "width 0.6s ease, background 0.6s ease",
          }}
        />
      </div>
    </div>
  );
}
