G='#1F8A5B'; GD='#156B45'; GL='#7FD3A8'; INK='#10231A'; PINK='#FF9DB0'; CREAM='#F6F5F0'
def eyes(kind):
    if kind=='happy':   # closed, smiling arcs
        return f'<path d="M48 96 q22 -26 44 0" fill="none" stroke="{INK}" stroke-width="9" stroke-linecap="round"/><path d="M108 96 q22 -26 44 0" fill="none" stroke="{INK}" stroke-width="9" stroke-linecap="round"/>'
    dx,dy,lid = {'open':(0,0,0),'side':(9,2,0),'sleepy':(0,6,1),'up':(0,-7,0)}[kind]
    s=''
    for cx in (70,130):
        s+=f'<ellipse cx="{cx}" cy="90" rx="27" ry="31" fill="#fff"/>'
        s+=f'<circle cx="{cx+dx}" cy="{94+dy}" r="14" fill="{INK}"/><circle cx="{cx+dx+5}" cy="{88+dy}" r="5" fill="#fff"/>'
        if lid: s+=f'<path d="M{cx-28} 90 a27 31 0 0 1 56 0 z" fill="{GD}"/><path d="M{cx-28} 90 h56" stroke="{INK}" stroke-width="5" stroke-linecap="round"/>'
    return s
def mouth(kind):
    return {'smile':f'<path d="M84 132 q16 16 32 0" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/>',
            'open':f'<path d="M80 128 q20 30 40 0 z" fill="{INK}"/><path d="M90 141 q10 8 20 0 q-10 -7 -20 0z" fill="{PINK}"/>',
            'smirk':f'<path d="M92 134 q16 8 28 -8" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/>',
            'tiny':f'<ellipse cx="100" cy="136" rx="7" ry="5" fill="{INK}"/>'}[kind]
def body(eye,mo,arms,extra='',legs=True,brows=''):
    L=''
    if legs:
        L=f'<path d="M72 196 v26" stroke="{GD}" stroke-width="16" stroke-linecap="round"/><path d="M128 196 v26" stroke="{GD}" stroke-width="16" stroke-linecap="round"/><ellipse cx="66" cy="226" rx="20" ry="10" fill="{INK}"/><ellipse cx="134" cy="226" rx="20" ry="10" fill="{INK}"/>'
    return f'''{L}{arms}
<rect x="0" y="0" width="200" height="200" rx="58" fill="{G}"/>
<path d="M0 142 v0 a58 58 0 0 0 58 58 h84 a58 58 0 0 0 58 -58 v-18 q-100 46 -200 0z" fill="{GD}" opacity=".35"/>
<path d="M150 0 h-6 l56 56 v-6 a50 50 0 0 0 -50 -50z" fill="{CREAM}"/><path d="M144 0 l56 56 h-34 a22 22 0 0 1 -22 -22z" fill="{GL}"/>
<path d="M62 2 q38 60 76 0" fill="none" stroke="{INK}" stroke-width="5" opacity=".0"/>
<ellipse cx="40" cy="124" rx="15" ry="9" fill="{PINK}" opacity=".75"/><ellipse cx="160" cy="124" rx="15" ry="9" fill="{PINK}" opacity=".75"/>
{eyes(eye)}{brows}{mouth(mo)}
<path d="M58 176 q42 26 84 0" fill="none" stroke="#fff" stroke-width="5" stroke-linecap="round" opacity=".9"/>
<rect x="84" y="182" width="32" height="40" rx="6" fill="#fff" stroke="{INK}" stroke-width="4"/><rect x="91" y="190" width="18" height="5" rx="2.5" fill="{G}"/><rect x="91" y="200" width="18" height="4" rx="2" fill="#C9CEC9"/><rect x="91" y="208" width="12" height="4" rx="2" fill="#C9CEC9"/>
{extra}'''
def arm(d): return f'<path d="{d}" fill="none" stroke="{GD}" stroke-width="16" stroke-linecap="round" stroke-linejoin="round"/>'
def hand(x,y): return f'<circle cx="{x}" cy="{y}" r="12" fill="{GD}"/>'
def g(x,y,s,inner,rot=0): return f'<g transform="translate({x} {y}) scale({s}) rotate({rot} 100 110)">{inner}</g>'
def shadow(x,y,w): return f'<ellipse cx="{x}" cy="{y}" rx="{w}" ry="{w*0.13:.0f}" fill="{INK}" opacity=".10"/>'

hello = body('open','open', arm('M4 120 q-30 16 -34 44')+hand(-30,166)+arm('M196 104 q34 -20 38 -62')+hand(234,40),
  extra=f'<path d="M252 18 l10 -12 M262 40 l16 -4 M240 6 l2 -16" stroke="{G}" stroke-width="6" stroke-linecap="round"/>')
nudge = body('side','smirk', arm('M4 124 q-28 20 -24 50')+hand(-20,176)+arm('M196 118 q40 4 66 -6')+hand(264,111)+f'<path d="M264 111 h34" stroke="{GD}" stroke-width="11" stroke-linecap="round"/>',
  brows=f'<path d="M46 52 q22 -14 46 -4" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/><path d="M110 44 q22 -10 44 6" fill="none" stroke="{INK}" stroke-width="7" stroke-linecap="round"/>')
yay = body('happy','open', arm('M6 96 q-34 -24 -40 -66')+hand(-36,28)+arm('M194 96 q34 -24 40 -66')+hand(236,28))
calm = body('sleepy','tiny', arm('M4 128 q-22 26 6 44')+hand(12,172)+arm('M196 128 q26 22 0 40')+hand(196,170), legs=False,
  extra=f'<g transform="translate(176 138)"><path d="M54 14 q26 4 22 26 q-4 16 -24 14" fill="none" stroke="{INK}" stroke-width="8"/><rect x="0" y="0" width="58" height="62" rx="12" fill="#fff" stroke="{INK}" stroke-width="5"/><path d="M12 -10 q-8 -12 2 -22 q8 -10 0 -22 M34 -10 q-8 -12 2 -22 q8 -10 0 -22" fill="none" stroke="#B9C2BC" stroke-width="5" stroke-linecap="round"/><path d="M20 22 l8 8 l14 -16" fill="none" stroke="{G}" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/></g>'
        +f'<path d="M34 196 q-26 10 -40 0 M166 196 q26 10 40 0" fill="none" stroke="{INK}" stroke-width="14" stroke-linecap="round"/>')
import random; random.seed(7)
pts=[(-180,-60),(-250,20),(-150,-190),(-90,-230),(-20,-250),(60,-240),(130,-200),(200,-160),(250,-70),(230,20),(-270,-80),(270,-130),(0,-200),(-210,90),(215,100),(-120,-150),(150,-120)]
conf=''.join(f'<rect x="{x-9}" y="{y-5}" width="18" height="10" rx="3" fill="{random.choice([GL,PINK,"#F5B942","#5B8DEF",G])}" transform="rotate({random.randint(0,180)} {x} {y})"/>' for x,y in pts)
def label(x,y,t,sub): return f'<text x="{x}" y="{y}" text-anchor="middle" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="30" font-weight="700" fill="{INK}">{t}</text><text x="{x}" y="{y+34}" text-anchor="middle" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="21" fill="#5B6B62">{sub}</text>'
def bubble(x,y,w,t): return f'<g transform="translate({x} {y})"><rect width="{w}" height="62" rx="20" fill="#fff" stroke="{INK}" stroke-width="4"/><path d="M34 60 l-14 22 l34 -22z" fill="#fff" stroke="{INK}" stroke-width="4" stroke-linejoin="round"/><rect x="24" y="54" width="34" height="8" fill="#fff"/><text x="{w/2}" y="40" text-anchor="middle" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="24" font-weight="600" fill="{INK}">{t}</text></g>'

sheet=f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1800 1180" width="1800" height="1180">
<rect width="1800" height="1180" fill="{CREAM}"/>
<text x="90" y="110" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="64" font-weight="800" fill="{INK}">Meet Nudge</text>
<text x="90" y="160" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="28" fill="#5B6B62">The PM OS mascot: a sticky note that grew legs, got a lanyard, and now follows up on everything.</text>
{shadow(330,640,150)}{g(190,300,1.4,hello)}{label(330,740,'“Hi! I read your Jira.”','Hello / onboarding')}
{shadow(800,640,150)}{g(650,300,1.4,nudge,-7)}{bubble(900,230,330,'psst… that draft?')}{label(800,740,'The gentle nudge','Reminders and follow-ups')}
{shadow(1420,640,150)}{g(1280,300,1.4,yay)}<g transform="translate(1420 430)">{conf}</g>{label(1420,740,'Approved!','Draft sent, tiny party')}
{shadow(330,1030,150)}{g(190,800,1.15,calm)}{label(330,1110,'Suspiciously calm','Empty state: nothing needs you')}
<g transform="translate(700 830)">
 <text font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="30" font-weight="700" fill="{INK}">In the app</text>
 <g transform="translate(0 30)"><rect width="150" height="150" rx="34" fill="{INK}"/>{g(35,30,0.4,body('open','smile','',legs=False))}</g>
 <g transform="translate(190 30)"><rect width="150" height="150" rx="34" fill="#fff" stroke="#DADDD6" stroke-width="3"/>{g(35,30,0.4,body('happy','smile','',legs=False))}</g>
 <g transform="translate(380 30)"><rect width="150" height="150" rx="34" fill="{GL}"/>{g(35,30,0.4,body('side','smirk','',legs=False))}</g>
 <text x="0" y="222" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="21" fill="#5B6B62">App icon, avatar, and badge sizes</text>
</g>
<g transform="translate(1320 840)">
 <text font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="30" font-weight="700" fill="{INK}">Colours</text>
 {''.join(f'<rect x="{i*84}" y="30" width="70" height="70" rx="16" fill="{c}" stroke="#DADDD6" stroke-width="2"/>' for i,c in enumerate([G,GD,GL,PINK,INK]))}
 <text x="0" y="140" font-family="Inter,Geist,Helvetica,Arial,sans-serif" font-size="21" fill="#5B6B62">The app’s green, plus a blush.</text>
</g>
</svg>'''
open('sheet.svg','w').write(sheet)
hero=f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 600" width="600" height="600"><rect width="600" height="600" rx="120" fill="{CREAM}"/>{shadow(300,520,150)}{g(160,170,1.4,hello)}</svg>'
open('nudge.svg','w').write(hero)
