import { useApp } from '../../app-context';
import { ReviewsInbox } from './Inbox';
import { ReviewCalm } from './parts';
import { ReviewPage } from './ReviewPage';

/**
 * The Reviews area: the inbox, or one draft. Its own chunk, fetched the first time a reviewer
 * opens it. Anyone without the reviewer role gets a calm sentence and no request: the server
 * would refuse them anyway (403 forbidden), an admin included.
 */
export function ReviewsPage({ id }: { id?: string }) {
  const { me } = useApp();
  if (me.reviewer !== true) return <ReviewCalm code="forbidden" />;
  return id === undefined ? <ReviewsInbox /> : <ReviewPage key={id} id={id} />;
}
