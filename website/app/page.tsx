import { Downloads } from "../components/Downloads";
import { Features } from "../components/Features";
import { Footer } from "../components/Footer";
import { Hero } from "../components/Hero";
import { MailingList } from "../components/MailingList";
import { OpenSource } from "../components/OpenSource";
import { ReleaseProvider } from "../components/ReleaseProvider";
import { WaveDivider } from "../components/Waveform";

export default function Home() {
  return (
    <ReleaseProvider>
      <a
        href="#download"
        className="sr-only focus:not-sr-only focus:absolute focus:left-4 focus:top-4 focus:z-50 focus:rounded-md focus:border focus:border-accent focus:bg-panel focus:px-4 focus:py-2"
      >
        Skip to downloads
      </a>
      <main>
        <Hero />
        <section className="bg-panel/35">
          <WaveDivider />
          <Features />
        </section>
        <section className="bg-bg">
          <WaveDivider />
          <Downloads />
        </section>
        <section className="bg-crust/30">
          <WaveDivider />
          <OpenSource />
        </section>
        <section className="bg-panel/35">
          <WaveDivider />
          <MailingList />
        </section>
      </main>
      <Footer />
    </ReleaseProvider>
  );
}
