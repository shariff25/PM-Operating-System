INK='#10231A'; PINK='#FF9DB0'
PAL={'nudge':('#1F8A5B','#156B45','#7FD3A8'),'chief':('#2F4FA8','#213A80','#9DB4F2'),'sprout':('#7DB928','#5C8F17','#C9EE8E'),'echo':('#F0654A','#C94A32','#FFC2B4')}
def face(k,eye='open',mo='smile'):
    G,GD,GL=PAL[k]
    e=''
    if eye=='happy':
        e=f'<path d="M48 96 q22 -26 44 0" fill="none" stroke="{INK}" stroke-width="9" stroke-linecap="round"/><path d="M108 96 q22 -26 44 0" fill="none" stroke="{INK}" stroke-width="9" stroke-linecap="round"/>'
    else:
        dx={'open':0,'side':9}[eye]
        for cx in (70,130):
            e+=f'<ellipse cx="{cx}" cy="90" rx="27" ry="31" fill="#fff"/><circle cx="{cx+dx}" cy="94" r="14" fill="{INK}"/><circle cx="{cx+dx+5}" cy="88" r="5" fill="#fff"/>'
    m={'smile':f'<path d="M84 132 q16 16 32 0" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/>',
       'open':f'<path d="M80 128 q20 30 40 0 z" fill="{INK}"/><path d="M90 141 q10 8 20 0 q-10 -7 -20 0z" fill="{PINK}"/>',
       'smirk':f'<path d="M92 134 q16 8 28 -8" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/>'}[mo]
    return f'''<rect width="200" height="200" rx="58" fill="{G}"/>
<path d="M0 142 a58 58 0 0 0 58 58 h84 a58 58 0 0 0 58 -58 v-18 q-100 46 -200 0z" fill="{GD}" opacity=".35"/>
<path d="M144 0 l56 56 h-34 a22 22 0 0 1 -22 -22z" fill="{GL}"/><path d="M150 0 h-6 l56 56 v-6 a50 50 0 0 0 -50 -50z" fill="VAR_BG"/>
<ellipse cx="40" cy="124" rx="15" ry="9" fill="{PINK}" opacity=".75"/><ellipse cx="160" cy="124" rx="15" ry="9" fill="{PINK}" opacity=".75"/>{e}{m}'''
def full(k,eye='open',mo='smile'):
    G,GD,GL=PAL[k]
    arm=lambda d:f'<path d="{d}" fill="none" stroke="{GD}" stroke-width="16" stroke-linecap="round"/>'
    hand=lambda x,y:f'<circle cx="{x}" cy="{y}" r="12" fill="{GD}"/>'
    legs=f'<path d="M72 196 v26 M128 196 v26" stroke="{GD}" stroke-width="16" stroke-linecap="round"/><ellipse cx="66" cy="226" rx="20" ry="10" fill="{INK}"/><ellipse cx="134" cy="226" rx="20" ry="10" fill="{INK}"/>'
    pre=legs; post=''
    if k=='nudge':
        pre+=arm('M4 120 q-30 16 -34 44')+hand(-30,166)+arm('M196 104 q34 -20 38 -62')+hand(234,40)
        post=f'<path d="M58 176 q42 26 84 0" fill="none" stroke="#fff" stroke-width="5" stroke-linecap="round"/><rect x="84" y="182" width="32" height="40" rx="6" fill="#fff" stroke="{INK}" stroke-width="4"/><rect x="91" y="190" width="18" height="5" rx="2.5" fill="{G}"/><rect x="91" y="200" width="18" height="4" rx="2" fill="#C9CEC9"/>'
    if k=='chief':
        pre+=arm('M4 120 q-30 16 -30 40')+hand(-26,162)+arm('M196 124 q22 14 14 40')+hand(210,166)
        post=(f'<path d="M100 150 l-13 12 l13 44 l13 -44z" fill="#F5B942" stroke="{INK}" stroke-width="4" stroke-linejoin="round"/><path d="M88 148 h24 l-5 14 h-14z" fill="#F5B942" stroke="{INK}" stroke-width="4" stroke-linejoin="round"/>'
              f'<g transform="translate(178 128) rotate(8)"><rect width="64" height="84" rx="8" fill="#fff" stroke="{INK}" stroke-width="5"/><rect x="20" y="-8" width="24" height="14" rx="5" fill="{INK}"/><path d="M12 26 l6 6 l10 -12 M12 46 l6 6 l10 -12" fill="none" stroke="{G}" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/><path d="M36 28 h16 M36 48 h16 M14 66 h38" stroke="#C9CEC9" stroke-width="5" stroke-linecap="round"/></g>')
    if k=='sprout':
        pre+=arm('M4 124 q-26 18 -26 44')+hand(-22,170)+arm('M196 108 q30 -14 40 -50')+hand(236,56)
        pre+=f'<path d="M100 4 v-40" stroke="{GD}" stroke-width="9" stroke-linecap="round"/><path d="M100 -30 q-34 -30 -52 -4 q26 22 52 4z" fill="{GL}" stroke="{GD}" stroke-width="5" stroke-linejoin="round"/><path d="M100 -34 q30 -34 54 -10 q-24 26 -54 10z" fill="{GL}" stroke="{GD}" stroke-width="5" stroke-linejoin="round"/>'
        post=f'<g transform="translate(214 -30)"><path d="M0 70 l22 -28 l16 12 l26 -40" fill="none" stroke="{INK}" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/><path d="M48 8 l20 2 l-4 20" fill="none" stroke="{INK}" stroke-width="8" stroke-linecap="round" stroke-linejoin="round"/></g>'
    if k=='echo':
        pre+=arm('M4 124 q-28 18 -26 46')+hand(-22,172)+arm('M196 116 q24 0 36 -10')
        post=(f'<g transform="translate(214 62) rotate(-14)"><path d="M0 26 l70 -30 v84 l-70 -30z" fill="#fff" stroke="{INK}" stroke-width="6" stroke-linejoin="round"/><rect x="-18" y="22" width="22" height="34" rx="6" fill="{INK}"/><path d="M84 12 q14 26 0 52 M98 0 q22 38 0 76" fill="none" stroke="{GD}" stroke-width="7" stroke-linecap="round"/></g>'+hand(226,108))
    return pre+face(k,eye,mo)+post
def svg(inner,w,h,vb,bg='#fff'): return f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{vb}">{inner.replace("VAR_BG",bg)}</svg>'
def big(k,size=120,bg='#fff',**kw): return svg(full(k,**kw),size*1.25,size,'-60 -70 380 320' if k in('sprout',) else '-60 -40 380 290',bg)
def small(k,size=28,bg='#fff',**kw): return svg(face(k,**kw),size,size,'0 0 200 200',bg)
import json
open('parts.json','w').write(json.dumps({
 'big':{k:big(k,132,'#ffffff',eye=e,mo=m) for k,e,m in [('nudge','open','open'),('chief','open','smile'),('sprout','happy','open'),('echo','side','smirk')]},
 'bigsoft':{k:big(k,132,'#fafaf7') for k in PAL},
 'small':{k:small(k,26) for k in PAL},'smalld':{k:small(k,26,'#0b0f0e') for k in PAL},'mid':{k:small(k,40) for k in PAL},
 'col':{k:PAL[k][0] for k in PAL}}))
