interface WelcomeProps {
  onNext: () => void;
}

export function Welcome({ onNext }: WelcomeProps) {
  return (
    <div className="step-container">
      <div className="welcome-logo">DCP &infin;</div>
      <h1 className="welcome-title">Turn your computer into an AI provider</h1>
      <p className="welcome-subtitle">Earn money while your PC idles</p>
      <button
        className="btn btn-primary btn-large"
        onClick={onNext}
        aria-label="Get started with DCP Provider setup"
      >
        Get Started
      </button>
    </div>
  );
}
