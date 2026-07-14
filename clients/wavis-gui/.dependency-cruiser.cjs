// Dependency-graph rules for wavis-gui. ESLint owns import fences
// (no-restricted-imports); this config owns graph-shape rules.
// Run: npx depcruise src --config .dependency-cruiser.cjs
module.exports = {
  forbidden: [
    {
      name: 'no-circular',
      comment:
        'Circular runtime imports make module initialization order fragile and hide ownership. Break the cycle by extracting the shared piece into its own module. Type-only cycles are excluded (erased at compile time).',
      severity: 'error',
      from: {},
      to: { circular: true, viaOnly: { dependencyTypesNot: ['type-only'] } },
    },
  ],
  options: {
    doNotFollow: { path: 'node_modules' },
    tsConfig: { fileName: 'tsconfig.json' },
    tsPreCompilationDeps: true,
    exclude: {
      path: '\\.(test|spec)\\.(ts|tsx)$|__tests__',
    },
  },
};
