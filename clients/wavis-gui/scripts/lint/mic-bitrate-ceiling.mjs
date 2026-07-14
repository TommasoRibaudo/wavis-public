import fs from 'node:fs';
import path from 'node:path';
import ts from 'typescript';

const TARGET_FILE = path.resolve('src/features/voice/livekit-media.ts');
const MIC_SOURCE_TEXT = 'Track.Source.Microphone';
const MIC_BITRATE_CEILING_BPS = 64_000;

function fail(message) {
  console.error(`[mic-bitrate-ceiling] ${message}`);
  process.exit(1);
}

function readSourceFile(filePath) {
  const sourceText = fs.readFileSync(filePath, 'utf8');
  if (!sourceText.includes(MIC_SOURCE_TEXT)) {
    fail(`expected at least one ${MIC_SOURCE_TEXT} publishTrack call in ${filePath}`);
  }
  return ts.createSourceFile(filePath, sourceText, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

function collectNumericConstants(sourceFile) {
  const constants = new Map();

  function visit(node) {
    if (ts.isVariableStatement(node)) {
      for (const declaration of node.declarationList.declarations) {
        if (
          ts.isIdentifier(declaration.name) &&
          declaration.initializer &&
          ts.isNumericLiteral(declaration.initializer)
        ) {
          constants.set(declaration.name.text, Number(declaration.initializer.text));
        }
      }
    }
    ts.forEachChild(node, visit);
  }

  visit(sourceFile);
  return constants;
}

function expressionToNumber(expression, numericConstants) {
  if (ts.isNumericLiteral(expression)) {
    return Number(expression.text.replaceAll('_', ''));
  }
  if (ts.isIdentifier(expression)) {
    return numericConstants.get(expression.text) ?? null;
  }
  return null;
}

function getPropertyByName(objectLiteral, propertyName) {
  return objectLiteral.properties.find((property) => {
    if (!ts.isPropertyAssignment(property)) {
      return false;
    }
    const name = property.name;
    return ts.isIdentifier(name) ? name.text === propertyName : name.getText() === propertyName;
  });
}

function unwrapExpression(expression) {
  let current = expression;
  while (
    ts.isAsExpression(current) ||
    ts.isTypeAssertionExpression(current) ||
    ts.isParenthesizedExpression(current) ||
    ts.isSatisfiesExpression?.(current)
  ) {
    current = current.expression;
  }
  return current;
}

function getLine(sourceFile, node) {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
}

function getPublishTrackViolations(sourceFile, numericConstants) {
  const violations = [];
  let microphonePublishCount = 0;

  function visit(node) {
    if (
      ts.isCallExpression(node) &&
      ts.isPropertyAccessExpression(node.expression) &&
      node.expression.name.text === 'publishTrack' &&
      node.arguments.length >= 2
    ) {
      const optionsArg = unwrapExpression(node.arguments[1]);
      if (!ts.isObjectLiteralExpression(optionsArg)) {
        ts.forEachChild(node, visit);
        return;
      }

      const sourceProperty = getPropertyByName(optionsArg, 'source');
      if (
        !sourceProperty ||
        !ts.isPropertyAssignment(sourceProperty) ||
        sourceProperty.initializer.getText(sourceFile) !== MIC_SOURCE_TEXT
      ) {
        ts.forEachChild(node, visit);
        return;
      }

      microphonePublishCount += 1;

      const bitrateProperty = getPropertyByName(optionsArg, 'audioBitrate');
      if (!bitrateProperty || !ts.isPropertyAssignment(bitrateProperty)) {
        violations.push(`missing audioBitrate at line ${getLine(sourceFile, node)}`);
        ts.forEachChild(node, visit);
        return;
      }

      const bitrateValue = expressionToNumber(bitrateProperty.initializer, numericConstants);
      if (bitrateValue === null) {
        violations.push(
          `unresolved audioBitrate expression "${bitrateProperty.initializer.getText(sourceFile)}" at line ${getLine(sourceFile, bitrateProperty.initializer)}`,
        );
      } else if (bitrateValue > MIC_BITRATE_CEILING_BPS) {
        violations.push(
          `audioBitrate ${bitrateValue} exceeds ${MIC_BITRATE_CEILING_BPS} at line ${getLine(sourceFile, bitrateProperty.initializer)}`,
        );
      }
    }

    ts.forEachChild(node, visit);
  }

  visit(sourceFile);

  if (microphonePublishCount === 0) {
    violations.push(`no publishTrack call with ${MIC_SOURCE_TEXT} found`);
  }

  return violations;
}

const sourceFile = readSourceFile(TARGET_FILE);
const numericConstants = collectNumericConstants(sourceFile);
const violations = getPublishTrackViolations(sourceFile, numericConstants);

if (violations.length > 0) {
  fail(violations.join('\n'));
}

console.log('[mic-bitrate-ceiling] OK');
