// These messages are visible in the conversation as well as read by the model.
const messages = {
  en: 'Tasty child status notifications (status changes are not proof of task success; inspect the results):',
  ko: 'Tasty 자식 상태 알림 (상태 변화는 작업 성공의 증거가 아닙니다. 결과를 확인하세요):',
  ja: 'Tasty 子セッションの状態通知（状態の変化は作業成功の証拠ではありません。結果を確認してください）:',
  'zh-Hans': 'Tasty 子会话状态通知（状态变化不能证明任务成功；检查结果）：',
};
export function heading(locale: string | undefined): string {
  return messages[locale as keyof typeof messages] ?? messages.en;
}
// Heading for lines the host writes when a queue message arrives.
const queueMessages = {
  en: "Tasty message queue notifications (messages are waiting in this surface's queue; read them with tasty read queue):",
  ko: 'Tasty 메시지 큐 알림 (이 surface의 큐에 메시지가 기다리고 있습니다. tasty read queue로 읽으세요):',
  ja: 'Tasty メッセージキュー通知（この surface のキューにメッセージが届いています。tasty read queue で読んでください）:',
  'zh-Hans': 'Tasty 消息队列通知（此 surface 的队列中有待读消息；用 tasty read queue 读取）：',
};
export function queueHeading(locale: string | undefined): string {
  return queueMessages[locale as keyof typeof queueMessages] ?? queueMessages.en;
}
// The host's untranslated ASCII tag at the start of a queue arrival line.
export const QUEUE_TAG = '[queue] ';
export function isQueueLine(line: string): boolean {
  return line.replace(/\r$/, '').startsWith(QUEUE_TAG);
}
// Queue lines only: the queue heading. No queue line: the status heading, as before.
// Both kinds: the status heading, then the queue heading; the lines keep log order.
export function headingFor(locale: string | undefined, body: string): string {
  const lines = body.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();
  const queue = lines.filter(isQueueLine).length;
  if (queue === 0) return heading(locale);
  if (queue === lines.length) return queueHeading(locale);
  return `${heading(locale)}\n${queueHeading(locale)}`;
}
// tasty-notify-gate strips this first line before the prompt enters.
export function delivery(seq: number, locale: string | undefined, body: string): string {
  return `[[tasty-notify ${seq}]]\n${headingFor(locale, body)}\n${body}`;
}
