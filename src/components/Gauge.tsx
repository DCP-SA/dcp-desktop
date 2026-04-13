import { useMemo } from "react";

interface GaugeProps {
  value: number;
  max: number;
  label: string;
  color: string;
  size?: number;
  unit?: string;
}

export function Gauge({
  value,
  max,
  label,
  color,
  size = 120,
  unit,
}: GaugeProps) {
  const percentage = useMemo(
    () => Math.min(Math.max((value / max) * 100, 0), 100),
    [value, max]
  );

  // Arc geometry: 270-degree sweep, gap at bottom
  const strokeWidth = size * 0.08;
  const radius = (size - strokeWidth) / 2;
  const center = size / 2;

  // Start at 135 degrees (bottom-left), sweep 270 degrees clockwise to 45 degrees (bottom-right)
  const startAngle = 135;
  const sweepAngle = 270;
  const endAngle = startAngle + sweepAngle;

  const circumference = 2 * Math.PI * radius;
  const arcLength = (sweepAngle / 360) * circumference;
  const filledLength = (percentage / 100) * arcLength;

  // Convert degrees to radians for SVG arc
  function polarToCartesian(cx: number, cy: number, r: number, angleDeg: number) {
    const angleRad = ((angleDeg - 90) * Math.PI) / 180;
    return {
      x: cx + r * Math.cos(angleRad),
      y: cy + r * Math.sin(angleRad),
    };
  }

  const arcStart = polarToCartesian(center, center, radius, startAngle);
  const arcEnd = polarToCartesian(center, center, radius, endAngle);
  const largeArcFlag = sweepAngle > 180 ? 1 : 0;

  const trackPath = [
    `M ${arcStart.x} ${arcStart.y}`,
    `A ${radius} ${radius} 0 ${largeArcFlag} 1 ${arcEnd.x} ${arcEnd.y}`,
  ].join(" ");

  // Display value: round to nearest integer for clean display
  const displayValue = Math.round(value);
  const displayUnit = unit ?? label;

  // Font sizes relative to gauge size
  const valueFontSize = size * 0.26;
  const unitFontSize = size * 0.12;

  return (
    <div
      className="gauge"
      style={{ width: size, height: size, position: "relative" }}
      role="meter"
      aria-valuenow={value}
      aria-valuemin={0}
      aria-valuemax={max}
      aria-label={`${label}: ${displayValue}`}
    >
      <svg
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        style={{ transform: "rotate(0deg)" }}
      >
        {/* Glow filter */}
        <defs>
          <filter id={`glow-${label.replace(/\W/g, "")}`} x="-50%" y="-50%" width="200%" height="200%">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feFlood floodColor={color} floodOpacity="0.4" result="color" />
            <feComposite in="color" in2="blur" operator="in" result="glow" />
            <feMerge>
              <feMergeNode in="glow" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>

        {/* Background track */}
        <path
          d={trackPath}
          fill="none"
          stroke="rgba(255,255,255,0.06)"
          strokeWidth={strokeWidth}
          strokeLinecap="round"
        />

        {/* Filled arc */}
        <path
          d={trackPath}
          fill="none"
          stroke={color}
          strokeWidth={strokeWidth}
          strokeLinecap="round"
          strokeDasharray={`${arcLength}`}
          strokeDashoffset={arcLength - filledLength}
          filter={`url(#glow-${label.replace(/\W/g, "")})`}
          style={{
            transition: "stroke-dashoffset 0.6s ease, stroke 0.6s ease",
          }}
        />
      </svg>

      {/* Center text */}
      <div
        className="gauge-text"
        style={{
          position: "absolute",
          top: "50%",
          left: "50%",
          transform: "translate(-50%, -45%)",
          textAlign: "center",
          lineHeight: 1,
        }}
      >
        <div
          className="gauge-value"
          style={{
            fontSize: valueFontSize,
            fontWeight: 700,
            color: "#F0F2F5",
            fontVariantNumeric: "tabular-nums",
            transition: "color 0.6s ease",
          }}
        >
          {displayValue}
        </div>
        <div
          className="gauge-unit"
          style={{
            fontSize: unitFontSize,
            fontWeight: 500,
            color: "rgba(123,143,163,0.9)",
            marginTop: 2,
          }}
        >
          {displayUnit}
        </div>
      </div>
    </div>
  );
}
