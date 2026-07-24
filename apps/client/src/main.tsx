import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

function isMobileBrowser() {
  if ("__TAURI_INTERNALS__" in window) return false;
  const navigatorWithHints = navigator as Navigator & { userAgentData?: { mobile?: boolean } };
  return navigatorWithHints.userAgentData?.mobile === true
    || /Android|iPhone|iPad|iPod|IEMobile|Opera Mini|Mobile/i.test(navigator.userAgent)
    || (/Macintosh/i.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
}

function MobileGate() {
  return (
    <main className="mobile-gate">
      <svg aria-hidden="true" width="58" height="58" viewBox="160 220 704 584" fill="none">
        <path d="M206 512h72l55-144 94 296 91-390 91 476 86-382 53 144h70" stroke="url(#mobile-noise-gradient)" strokeWidth="64" strokeLinecap="round" strokeLinejoin="round" />
        <defs><linearGradient id="mobile-noise-gradient" x1="214" y1="512" x2="810" y2="512"><stop stopColor="#a995ff" /><stop offset="1" stopColor="#5d40d2" /></linearGradient></defs>
      </svg>
      <h1>noise</h1>
      <p>noise for the web is built for desktop and laptop screens right now.</p>
      <strong>Open this page on a computer, or download the desktop app.</strong>
      <div>
        <a href="https://github.com/GnosysLabs/noise/releases/latest">download for Mac</a>
        <a href="https://github.com/GnosysLabs/noise/releases/latest">download for Windows</a>
      </div>
      <small>mobile is coming later</small>
    </main>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  isMobileBrowser() ? <MobileGate /> : <App />,
);
