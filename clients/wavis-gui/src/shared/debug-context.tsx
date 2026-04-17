import { createContext, useContext, useState, type ReactNode } from 'react';

interface DebugContextValue {
  showSecrets: boolean;
  setShowSecrets: (v: boolean) => void;
}

const DebugContext = createContext<DebugContextValue>({
  showSecrets: false,
  setShowSecrets: () => {},
});

export function DebugProvider({ children }: { children: ReactNode }) {
  const [showSecrets, setShowSecrets] = useState(false);
  return (
    <DebugContext.Provider value={{ showSecrets, setShowSecrets }}>
      {children}
    </DebugContext.Provider>
  );
}

export function useDebug() {
  return useContext(DebugContext);
}
