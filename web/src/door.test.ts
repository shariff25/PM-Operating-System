import { describe, expect, it } from 'vitest';
import { adminDoorHref, adminHere, onAdminDoor } from './door';

describe('the two doors (slice H10)', () => {
  it('knows the admin door, and treats an older server as one door', () => {
    expect(onAdminDoor({ door: 'admin' })).toBe(true);
    expect(onAdminDoor({ door: 'public' })).toBe(false);
    expect(onAdminDoor({})).toBe(false);
    expect(adminHere({ door: 'admin' })).toBe(true);
    expect(adminHere({})).toBe(true);
    expect(adminHere({ door: 'public' })).toBe(false);
  });

  it("links an admin on the public door to the admin door's address, and only to http or https", () => {
    expect(adminDoorHref({ door: 'public', admin_url: 'https://pmos-admin.corp.example' })).toBe(
      'https://pmos-admin.corp.example/#/admin',
    );
    expect(adminDoorHref({ door: 'public', admin_url: 'http://pmos-prod.localhost:8772' }, '#/admin/audit')).toBe(
      'http://pmos-prod.localhost:8772/#/admin/audit',
    );
    expect(adminDoorHref({ door: 'public', admin_url: 'javascript:alert(1)' })).toBeNull();
    expect(adminDoorHref({ door: 'public' })).toBeNull();
    expect(adminDoorHref({ door: 'admin', admin_url: 'https://x.example' })).toBeNull();
  });
});
