import { isMacos } from './platform';

function assertEqual(actual: unknown, expected: unknown, msg?: string): void {
  if (!Object.is(actual, expected)) {
    throw new Error(`${msg ?? ''} Expected ${String(expected)}, got ${String(actual)}`);
  }
}

assertEqual(isMacos('MacIntel'), true, 'MacIntel is mac');
assertEqual(isMacos('Macintosh'), true, 'Macintosh is mac');
assertEqual(isMacos('Win32'), false, 'Win32 not mac');
assertEqual(isMacos('Linux x86_64'), false, 'Linux not mac');
assertEqual(isMacos(''), false, 'empty not mac');

console.log('platform.test.ts passed');
