import { ApiError } from './api/types';

/**
 * Was the request definitely refused? Only then is it known that nothing was done: the server
 * answered with a 4xx and meant it. A dropped connection, a timeout (also a 408), a 5xx, a
 * reply that cannot be read, or the server's own `unknown_outcome` leaves it open whether the
 * write went through, and the app must not say either way.
 */
export function isDefiniteRefusal(e: unknown): e is ApiError {
  return e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 408 && e.code !== 'unknown_outcome';
}
