# Unit data (UDTA) and ALOW dump

Source: PUDs in D:/Tools/wc2mod/triggers (maps and reference/tr). Layout: offsets from `pudvalidate.rs` `UDTA_RULES`. Costs shown x10 as in game (stored /10). Build time in game cycles.

Every one of the 7 distinct UDTA blocks across 9 maps is different, so custom rules are already in wide use.

## Baseline (mutton thinned / sheep tag, identical)

| id | unit | HP | sight | mana | build | gold | lumber | oil | range | armor | dmg | pierce |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | footman | 60 | 4 | 0 | 60 | 600 | 0 | 0 | 1 | 2 | 6 | 3 |
| 1 | grunt | 60 | 4 | 0 | 60 | 600 | 0 | 0 | 1 | 2 | 6 | 3 |
| 2 | peasant | 30 | 4 | 0 | 45 | 400 | 0 | 0 | 1 | 0 | 3 | 2 |
| 3 | peon | 30 | 4 | 0 | 45 | 400 | 0 | 0 | 1 | 0 | 3 | 2 |
| 4 | ballista | 110 | 9 | 0 | 250 | 900 | 300 | 0 | 8 | 0 | 80 | 0 |
| 5 | catapult | 110 | 9 | 0 | 250 | 900 | 300 | 0 | 8 | 0 | 80 | 0 |
| 6 | knight | 90 | 4 | 0 | 90 | 800 | 100 | 0 | 1 | 4 | 8 | 4 |
| 7 | ogre | 90 | 4 | 0 | 90 | 800 | 100 | 0 | 1 | 4 | 8 | 4 |
| 8 | archer | 40 | 5 | 0 | 70 | 500 | 50 | 0 | 4 | 0 | 3 | 6 |
| 9 | axethrower | 40 | 5 | 0 | 70 | 500 | 50 | 0 | 4 | 0 | 3 | 6 |
| 10 | mage | 60 | 9 | 1 | 120 | 1200 | 0 | 0 | 2 | 0 | 0 | 9 |
| 11 | deathknight | 60 | 9 | 1 | 120 | 1200 | 0 | 0 | 3 | 0 | 0 | 9 |
| 12 | paladin | 90 | 5 | 0 | 90 | 800 | 100 | 0 | 1 | 4 | 8 | 4 |
| 13 | ogremage | 90 | 5 | 0 | 90 | 800 | 100 | 0 | 1 | 4 | 8 | 4 |
| 14 | dwarves | 40 | 4 | 0 | 200 | 700 | 250 | 0 | 1 | 0 | 4 | 2 |
| 15 | sappers | 40 | 4 | 0 | 200 | 700 | 250 | 0 | 1 | 0 | 4 | 2 |
| 16 | atkpeasant | 30 | 4 | 0 | 45 | 400 | 0 | 0 | 1 | 0 | 3 | 2 |
| 17 | atkpeon | 30 | 4 | 0 | 45 | 400 | 0 | 0 | 1 | 0 | 3 | 2 |
| 18 | ranger | 50 | 6 | 0 | 70 | 500 | 50 | 0 | 4 | 0 | 3 | 6 |
| 19 | berserker | 50 | 6 | 0 | 70 | 500 | 50 | 0 | 4 | 0 | 3 | 6 |
| 26 | humtanker | 90 | 4 | 0 | 50 | 400 | 200 | 0 | 1 | 10 | 0 | 0 |
| 27 | orctanker | 90 | 4 | 0 | 50 | 400 | 200 | 0 | 1 | 10 | 0 | 0 |
| 28 | humtransport | 150 | 4 | 0 | 70 | 600 | 200 | 500 | 1 | 0 | 0 | 0 |
| 29 | orctransport | 150 | 4 | 0 | 70 | 600 | 200 | 500 | 1 | 0 | 0 | 0 |
| 30 | elvdestroyer | 100 | 8 | 0 | 90 | 700 | 350 | 700 | 4 | 10 | 35 | 0 |
| 31 | trolldestroyer | 100 | 8 | 0 | 90 | 700 | 350 | 700 | 4 | 10 | 35 | 0 |
| 32 | battleship | 150 | 8 | 0 | 140 | 1000 | 500 | 1000 | 6 | 15 | 130 | 0 |
| 33 | juggernaut | 150 | 8 | 0 | 140 | 1000 | 500 | 1000 | 6 | 15 | 130 | 0 |
| 36 | u36 | 60 | 4 | 0 | 60 | 400 | 0 | 0 | 1 | 2 | 9 | 1 |
| 37 | u37 | 60 | 4 | 0 | 60 | 400 | 0 | 0 | 1 | 2 | 9 | 1 |
| 38 | gnomsub | 60 | 5 | 0 | 100 | 800 | 150 | 900 | 4 | 0 | 50 | 0 |
| 39 | turtle | 60 | 5 | 0 | 100 | 800 | 150 | 900 | 4 | 0 | 50 | 0 |
| 40 | flyingmachine | 150 | 9 | 0 | 65 | 500 | 100 | 0 | 1 | 2 | 0 | 0 |
| 41 | zeppelin | 150 | 9 | 0 | 65 | 500 | 100 | 0 | 1 | 2 | 0 | 0 |
| 42 | gryphon | 100 | 6 | 0 | 250 | 2500 | 0 | 0 | 4 | 5 | 0 | 16 |
| 43 | dragon | 100 | 6 | 0 | 250 | 2500 | 0 | 0 | 4 | 5 | 0 | 16 |
| 45 | eye | 100 | 3 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 |
| 49 | chogall | 100 | 5 | 0 | 100 | 1100 | 50 | 0 | 1 | 0 | 10 | 5 |
| 50 | lothar | 90 | 5 | 0 | 100 | 900 | 100 | 0 | 1 | 4 | 8 | 4 |
| 51 | guldan | 40 | 8 | 1 | 120 | 1200 | 0 | 0 | 3 | 0 | 0 | 3 |
| 52 | uther | 90 | 5 | 0 | 100 | 900 | 100 | 0 | 1 | 4 | 8 | 4 |
| 53 | zuljin | 40 | 6 | 0 | 70 | 500 | 50 | 0 | 5 | 0 | 3 | 6 |
| 55 | skeleton | 40 | 3 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 6 | 3 |
| 56 | daemon | 60 | 5 | 0 | 0 | 0 | 0 | 0 | 2 | 2 | 9 | 1 |
| 57 | critter | 5 | 2 | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 0 | 0 |
| 58 | farm | 400 | 3 | 0 | 100 | 500 | 250 | 0 | 0 | 20 | 0 | 0 |
| 59 | pigfarm | 400 | 3 | 0 | 100 | 500 | 250 | 0 | 0 | 20 | 0 | 0 |
| 60 | humbarracks | 800 | 3 | 0 | 200 | 700 | 450 | 0 | 0 | 20 | 0 | 0 |
| 61 | orcbarracks | 800 | 3 | 0 | 200 | 700 | 450 | 0 | 0 | 20 | 0 | 0 |
| 62 | church | 700 | 3 | 0 | 175 | 900 | 500 | 0 | 0 | 20 | 0 | 0 |
| 63 | altar | 700 | 3 | 0 | 175 | 900 | 500 | 0 | 0 | 20 | 0 | 0 |
| 64 | humscout | 100 | 9 | 0 | 60 | 550 | 200 | 0 | 0 | 20 | 0 | 0 |
| 65 | orcscout | 100 | 9 | 0 | 60 | 550 | 200 | 0 | 0 | 20 | 0 | 0 |
| 66 | stables | 500 | 3 | 0 | 150 | 1000 | 300 | 0 | 0 | 20 | 0 | 0 |
| 67 | ogremound | 500 | 3 | 0 | 150 | 1000 | 300 | 0 | 0 | 20 | 0 | 0 |
| 68 | inventor | 500 | 3 | 0 | 150 | 1000 | 400 | 0 | 0 | 20 | 0 | 0 |
| 69 | alchemist | 500 | 3 | 0 | 150 | 1000 | 400 | 0 | 0 | 20 | 0 | 0 |
| 70 | aviary | 500 | 3 | 0 | 150 | 1000 | 400 | 0 | 0 | 20 | 0 | 0 |
| 71 | roost | 500 | 3 | 0 | 150 | 1000 | 400 | 0 | 0 | 20 | 0 | 0 |
| 72 | humshipyard | 1100 | 3 | 0 | 200 | 800 | 450 | 0 | 0 | 20 | 0 | 0 |
| 73 | orcshipyard | 1100 | 3 | 0 | 200 | 800 | 450 | 0 | 0 | 20 | 0 | 0 |
| 74 | townhall | 1200 | 4 | 0 | 255 | 1200 | 800 | 0 | 0 | 20 | 0 | 0 |
| 75 | greathall | 1200 | 4 | 0 | 255 | 1200 | 800 | 0 | 0 | 20 | 0 | 0 |
| 76 | elvlumber | 600 | 3 | 0 | 150 | 600 | 450 | 0 | 0 | 20 | 0 | 0 |
| 77 | trolllumber | 600 | 3 | 0 | 150 | 600 | 450 | 0 | 0 | 20 | 0 | 0 |
| 78 | humfoundry | 750 | 3 | 0 | 175 | 700 | 400 | 400 | 0 | 20 | 0 | 0 |
| 79 | orcfoundry | 750 | 3 | 0 | 175 | 700 | 400 | 400 | 0 | 20 | 0 | 0 |
| 80 | magetower | 500 | 3 | 0 | 125 | 1000 | 200 | 0 | 0 | 20 | 0 | 0 |
| 81 | temple | 500 | 3 | 0 | 125 | 1000 | 200 | 0 | 0 | 20 | 0 | 0 |
| 82 | humsmith | 775 | 3 | 0 | 200 | 800 | 450 | 100 | 0 | 20 | 0 | 0 |
| 83 | orcsmith | 775 | 3 | 0 | 200 | 800 | 450 | 100 | 0 | 20 | 0 | 0 |
| 84 | humrefinery | 600 | 3 | 0 | 225 | 800 | 350 | 200 | 0 | 20 | 0 | 0 |
| 85 | orcrefinery | 600 | 3 | 0 | 225 | 800 | 350 | 200 | 0 | 20 | 0 | 0 |
| 86 | humoilplat | 650 | 3 | 0 | 200 | 700 | 450 | 0 | 0 | 20 | 0 | 0 |
| 87 | orcoilplat | 650 | 3 | 0 | 200 | 700 | 450 | 0 | 0 | 20 | 0 | 0 |
| 88 | keep | 1400 | 6 | 0 | 200 | 2000 | 1000 | 200 | 0 | 20 | 0 | 0 |
| 89 | stronghold | 1400 | 6 | 0 | 200 | 2000 | 1000 | 200 | 0 | 20 | 0 | 0 |
| 90 | castle | 1600 | 9 | 0 | 200 | 2500 | 1200 | 500 | 0 | 20 | 0 | 0 |
| 91 | fortress | 1600 | 9 | 0 | 200 | 2500 | 1200 | 500 | 0 | 20 | 0 | 0 |
| 92 | goldmine | 25500 | 3 | 0 | 150 | 0 | 0 | 0 | 0 | 20 | 0 | 0 |
| 96 | humguard | 130 | 9 | 0 | 140 | 500 | 150 | 0 | 6 | 20 | 4 | 12 |
| 97 | orcguard | 130 | 9 | 0 | 140 | 500 | 150 | 0 | 6 | 20 | 4 | 12 |
| 98 | humcannon | 160 | 9 | 0 | 190 | 1000 | 300 | 0 | 7 | 20 | 50 | 0 |
| 99 | orccannon | 160 | 9 | 0 | 190 | 1000 | 300 | 0 | 7 | 20 | 50 | 0 |
| 101 | portal | 5000 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 102 | runestone | 5000 | 4 | 0 | 175 | 900 | 500 | 0 | 0 | 20 | 0 | 0 |
| 103 | humwall | 40 | 1 | 0 | 30 | 20 | 10 | 0 | 0 | 0 | 0 | 0 |
| 104 | orcwall | 40 | 1 | 0 | 30 | 20 | 10 | 0 | 0 | 0 | 0 | 0 |
| 105 | id105 | 255 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 106 | id106 | 255 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 107 | id107 | 255 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 108 | id108 | 255 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| 109 | id109 | 255 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

## Per-map changes vs baseline

### capture flag v3.pud

23 unit types changed.

- 2 peasant: sight 4->9, build 45->10, gold 40->0, wup 0->1, aup 0->1
- 3 peon: sight 4->9, build 45->10, gold 40->0, wup 0->1, aup 0->1
- 20 alleria: sight 0->9, hp 0->1, build 0->30, range 0->7, reactAI 0->7, reactHu 0->5, armor 0->5, select 0->1 ...
- 21 teron: sight 0->9, hp 0->180, mana 0->1, build 0->120, gold 0->120, range 0->4, reactAI 0->11, reactHu 0->9 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->6, hp 0->300, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->6 ...
- 24 khadgar: sight 0->9, mana 0->1, build 0->30, range 0->8, reactAI 0->11, reactHu 0->9, select 0->1, prio 0->70 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 44 turalyon: sight 0->6, hp 0->180, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->7, reactHu 0->5 ...
- 45 eye: sight 3->9, decay 3->1, flags 134217858->130
- 46 danath: sight 0->6, hp 0->220, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 49 chogall: sight 5->9, hp 100->1, build 100->50, gold 110->0, lumber 5->0, dmg 10->0, pierce 5->8
- 74 townhall: sight 4->9, hp 1200->9999, build 255->1, gold 120->0, lumber 80->0, oil 0->10
- 75 greathall: sight 4->9, hp 1200->9999, build 255->1, gold 120->0, lumber 80->0, oil 0->10
- 82 humsmith: sight 3->9, hp 775->9999, build 200->1, gold 80->0, lumber 45->0, oil 10->1
- 83 orcsmith: sight 3->9, hp 775->9999, build 200->1, gold 80->0, lumber 45->0, oil 10->1
- 57 critter: hp 5->50
- 58 farm: hp 400->9999, build 100->1, gold 50->10, lumber 25->15, oil 0->1
- 59 pigfarm: hp 400->9999, build 100->1, gold 50->10, lumber 25->15, oil 0->1
- 103 humwall: build 30->5, gold 2->0
- 104 orcwall: build 30->5, gold 2->0

### desert strike v4.pud

80 unit types changed.

- 0 footman: sight 4->9, hp 60->160
- 1 grunt: sight 4->9, hp 60->160
- 2 peasant: sight 4->9, gold 40->250, lumber 0->250, wup 0->1, aup 0->1
- 3 peon: sight 4->9, gold 40->250, lumber 0->250, wup 0->1, aup 0->1
- 6 knight: sight 4->9, hp 90->290
- 7 ogre: sight 4->9, hp 90->290
- 8 archer: sight 5->9, aup 0->1
- 9 axethrower: sight 5->9, aup 0->1
- 12 paladin: sight 5->9
- 13 ogremage: sight 5->9
- 14 dwarves: sight 4->9
- 15 sappers: sight 4->9
- 16 atkpeasant: sight 4->9, hp 30->300
- 17 atkpeon: sight 4->9, hp 30->300
- 18 ranger: sight 6->9, aup 0->1
- 19 berserker: sight 6->9, aup 0->1
- 20 alleria: sight 0->9, hp 0->120, build 0->70, gold 0->50, lumber 0->5, range 0->7, reactAI 0->7, reactHu 0->5 ...
- 21 teron: sight 0->9, hp 0->1, mana 0->1, build 0->1, gold 0->220, lumber 0->80, oil 0->1, range 0->1 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->9, hp 0->400, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->4, reactAI 0->7 ...
- 24 khadgar: sight 0->9, hp 0->120, mana 0->1, build 0->120, gold 0->120, range 0->6, reactAI 0->11, reactHu 0->9 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 44 turalyon: sight 0->9, hp 0->400, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->4, reactAI 0->7 ...
- 46 danath: sight 0->6, hp 0->220, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 55 skeleton: sight 3->9, hp 40->80, wup 0->1, aup 0->1
- 56 daemon: sight 5->9, hp 60->666, range 2->3, armor 2->50, select 0->1, dmg 9->10, pierce 1->30, wup 0->1 ...
- 100 circle: sight 0->9, hp 0->1, gold 0->10, lumber 0->200
- 101 portal: sight 0->9, hp 5000->16000, build 0->1, gold 0->100, lumber 0->50, oil 0->1, armor 0->100
- 102 runestone: sight 4->9, build 175->1, gold 90->200, lumber 50->200, oil 0->1
- 4 ballista: hp 110->510
- 5 catapult: hp 110->510
- 10 mage: hp 60->260
- 11 deathknight: hp 60->260
- 30 elvdestroyer: hp 100->50, dmg 35->20, pierce 0->5, type 2->1, flags 524296->524290
- 31 trolldestroyer: hp 100->50, dmg 35->20, pierce 0->5, type 2->1, flags 524296->524290
- 32 battleship: hp 150->70, dmg 130->40, missile 7->1, type 2->1, cantarget 3->7, flags 540680->540674
- 33 juggernaut: hp 150->70, dmg 130->40, missile 7->2, type 2->1, cantarget 3->7, flags 540680->540674
- 38 gnomsub: hp 60->300, dmg 50->255, pierce 0->255, type 2->1, cantarget 2->7, flags 524488->524482
- 39 turtle: hp 60->300, dmg 50->255, pierce 0->255, type 2->1, cantarget 2->7, flags 524488->524482
- 42 gryphon: hp 100->350, pierce 16->32, wup 0->1, aup 0->1
- 43 dragon: hp 100->350, pierce 16->32, wup 0->1, aup 0->1
- 58 farm: hp 400->4000, build 100->15, lumber 25->0
- 59 pigfarm: hp 400->4000, build 100->15, lumber 25->0
- 60 humbarracks: hp 800->1, build 200->3, gold 70->30, lumber 45->5
- 61 orcbarracks: hp 800->1, build 200->3, gold 70->30, lumber 45->5
- 62 church: hp 700->1, build 175->5, lumber 50->30
- 63 altar: hp 700->1, build 175->5, lumber 50->30
- 66 stables: hp 500->1, build 150->4, gold 100->60, lumber 30->35
- 67 ogremound: hp 500->1, build 150->4, gold 100->60, lumber 30->35
- 68 inventor: hp 500->1, build 150->1, gold 100->200, lumber 40->100, oil 0->1
- 69 alchemist: hp 500->1, build 150->1, gold 100->200, lumber 40->100, oil 0->1
- 70 aviary: hp 500->1, build 150->5, gold 100->90, lumber 40->50
- 71 roost: hp 500->1, build 150->5, gold 100->90, lumber 40->50
- 72 humshipyard: hp 1100->1, build 200->3, gold 80->10, lumber 45->10
- 73 orcshipyard: hp 1100->1, build 200->3, gold 80->10, lumber 45->10
- 74 townhall: hp 1200->1, build 255->30, gold 120->40, lumber 80->10
- 75 greathall: hp 1200->1, build 255->30, gold 120->40, lumber 80->10
- 76 elvlumber: hp 600->1, build 150->3, gold 60->80, lumber 45->20

### edo def v1.4.pud

58 unit types changed.

- 2 peasant: sight 4->9, gold 40->1
- 20 alleria: sight 0->9, hp 0->120, build 0->70, gold 0->3, range 0->11, reactAI 0->7, reactHu 0->5, armor 0->5 ...
- 21 teron: sight 0->9, hp 0->100, mana 0->1, build 0->120, gold 0->2, lumber 0->1, range 0->10, reactAI 0->11 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->9, hp 0->300, mana 0->1, build 0->90, gold 0->4, range 0->8, reactAI 0->6, reactHu 0->4 ...
- 24 khadgar: sight 0->9, hp 0->100, mana 0->1, build 0->120, gold 0->5, range 0->10, reactAI 0->11, reactHu 0->9 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 44 turalyon: sight 0->6, hp 0->180, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->7, reactHu 0->5 ...
- 45 eye: sight 3->9, flags 134217858->130
- 46 danath: sight 0->6, hp 0->220, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 55 skeleton: sight 3->9, range 1->10, pierce 3->13, missile 29->16, decay 100->5, cantarget 1->7
- 57 critter: sight 2->9
- 0 footman: hp 60->100
- 4 ballista: hp 110->700, gold 90->2, lumber 30->0
- 5 catapult: hp 110->20000
- 6 knight: hp 90->160
- 7 ogre: hp 90->100
- 8 archer: hp 40->80
- 10 mage: hp 60->666, gold 120->4
- 11 deathknight: hp 60->500
- 12 paladin: hp 90->275
- 13 ogremage: hp 90->400
- 14 dwarves: hp 40->200
- 15 sappers: hp 40->180
- 17 atkpeon: hp 30->10
- 18 ranger: hp 50->350
- 19 berserker: hp 50->250
- 26 humtanker: hp 90->30, type 2->1, flags 520->2
- 27 orctanker: hp 90->50, type 2->1, flags 520->2
- 28 humtransport: hp 150->10, type 2->1, flags 1032->2
- 29 orctransport: hp 150->20, type 2->1, flags 1032->2
- 31 trolldestroyer: hp 100->200, type 2->1, flags 524296->524290
- 32 battleship: hp 150->800, type 2->1, flags 540680->540674
- 33 juggernaut: hp 150->1500, type 2->1, flags 540680->540674
- 38 gnomsub: hp 60->400, type 2->1, flags 524488->524482
- 39 turtle: hp 60->600, type 2->1, flags 524488->524482
- 42 gryphon: hp 100->250
- 43 dragon: hp 100->350
- 51 guldan: hp 40->1000
- 52 uther: hp 90->700
- 56 daemon: hp 60->666
- 40 flyingmachine: build 65->1, gold 50->0, lumber 10->1
- 60 humbarracks: build 200->20, gold 70->1, lumber 45->1
- 64 humscout: build 60->10, gold 55->2, lumber 20->0
- 66 stables: build 150->20, gold 100->2, lumber 30->0
- 68 inventor: build 150->50, gold 100->1, lumber 40->0
- 74 townhall: build 255->1, gold 120->0, lumber 80->1
- 76 elvlumber: build 150->5, gold 60->1, lumber 45->0
- 82 humsmith: build 200->20, gold 80->1, lumber 45->1, oil 10->0
- 88 keep: build 200->255, gold 200->0, lumber 100->1, oil 20->0
- 90 castle: build 200->255, gold 250->0, lumber 120->1, oil 50->0
- 96 humguard: build 140->10, gold 50->0, lumber 15->0, range 6->10, wup 0->1
- 98 humcannon: build 190->20, gold 100->2, lumber 30->0, range 7->11, dmg 50->140, wup 0->1, cantarget 3->7
- 80 magetower: gold 100->4, lumber 20->1
- 103 humwall: oil 0->255
- 30 elvdestroyer: type 2->1, flags 524296->524290

### hero surv v1.3.pud

51 unit types changed.

- 0 footman: sight 4->9, hp 60->80, armor 2->0, dmg 6->3
- 1 grunt: sight 4->9, hp 60->30, build 60->0, gold 60->0, oil 0->3, armor 2->1, dmg 6->3, pierce 3->1
- 2 peasant: sight 4->9, gold 40->0, lumber 0->1, dmg 3->0, pierce 2->1, wup 0->1, aup 0->1
- 6 knight: sight 4->9
- 7 ogre: sight 4->9, hp 90->50, build 90->0, gold 80->0, lumber 10->0, oil 0->5, armor 4->0, pierce 4->1
- 8 archer: sight 5->9, hp 40->60, range 4->6, dmg 3->0, pierce 6->8, aup 0->1
- 9 axethrower: sight 5->9, hp 40->25, dmg 3->4, pierce 6->2, aup 0->1
- 12 paladin: sight 5->9, hp 90->120, dmg 8->9, pierce 4->6
- 13 ogremage: sight 5->9, hp 90->87, build 90->0, gold 80->0, lumber 10->0, oil 0->5, pierce 4->2
- 15 sappers: sight 4->9, hp 40->50, armor 0->40, dmg 4->0, pierce 2->0, aup 0->1, flags 168296449->167772161
- 16 atkpeasant: sight 4->9, hp 30->1, dmg 3->0, pierce 2->0, wup 0->1, aup 0->1, flags 134742017->134217729
- 17 atkpeon: sight 4->9, hp 30->15, dmg 3->0, pierce 2->1, wup 0->1, aup 0->1
- 18 ranger: sight 6->9, hp 50->120, range 4->8, armor 0->3, dmg 3->5, pierce 6->10, aup 0->1
- 19 berserker: sight 6->9, aup 0->1
- 20 alleria: sight 0->9, hp 0->120, build 0->70, gold 0->50, lumber 0->5, range 0->7, reactAI 0->7, reactHu 0->5 ...
- 21 teron: sight 0->9, hp 0->180, mana 0->1, build 0->120, gold 0->120, range 0->4, reactAI 0->11, reactHu 0->9 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->6, hp 0->300, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->6 ...
- 24 khadgar: sight 0->9, hp 0->120, mana 0->1, build 0->120, gold 0->120, range 0->6, reactAI 0->11, reactHu 0->9 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 42 gryphon: sight 6->9, hp 100->80, range 4->7, armor 5->2, pierce 16->18, wup 0->1, aup 0->1
- 44 turalyon: sight 0->9, hp 0->170, build 0->90, gold 0->80, lumber 0->10, range 0->2, reactAI 0->7, reactHu 0->5 ...
- 46 danath: sight 0->9, hp 0->140, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->4 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 50 lothar: sight 5->9, hp 90->150, armor 4->6, dmg 8->10, pierce 4->8
- 52 uther: sight 5->9, hp 90->120, armor 4->5, dmg 8->10
- 57 critter: sight 2->9, hp 5->2
- 58 farm: sight 3->9
- 60 humbarracks: sight 3->9
- 61 orcbarracks: sight 3->9
- 66 stables: sight 3->9
- 82 humsmith: sight 3->9
- 101 portal: sight 0->9, hp 5000->1000, reactAI 0->10, prio 0->80, annoy 0->60
- 102 runestone: sight 4->9, hp 5000->300, gold 90->0, lumber 50->2
- 3 peon: hp 30->1, dmg 3->0, pierce 2->0, flags 134742273->134217985
- 4 ballista: hp 110->120, range 8->12
- 5 catapult: hp 110->60, range 8->12, dmg 80->60
- 10 mage: hp 60->100, range 2->7, pierce 9->10, wup 0->1, aup 0->1
- 43 dragon: hp 100->50, range 4->6, armor 5->2, pierce 16->10, wup 0->1, aup 0->1
- 51 guldan: hp 40->100, range 3->5, armor 0->8, pierce 3->10, wup 0->1, aup 0->1
- 59 pigfarm: hp 400->150
- 96 humguard: hp 130->100, build 140->50, gold 50->0, lumber 15->0, range 6->7, dmg 4->0, pierce 12->5, wup 0->1
- 98 humcannon: hp 160->200, build 190->50, gold 100->0, lumber 30->0, dmg 50->30, wup 0->1
- 64 humscout: build 60->50, gold 55->0, lumber 20->4
- 65 orcscout: gold 55->255, lumber 20->255, oil 0->255
- 103 humwall: gold 2->255, lumber 1->255, oil 0->255
- 104 orcwall: gold 2->255, lumber 1->255, oil 0->255
- 55 skeleton: armor 0->2, pierce 3->4, wup 0->1, aup 0->1
- 14 dwarves: aup 0->1
- 45 eye: flags 134217858->130

### smart dota v1.4.pud

22 unit types changed.

- 20 alleria: sight 0->9, hp 0->120, build 0->70, gold 0->50, lumber 0->5, range 0->7, reactAI 0->7, reactHu 0->5 ...
- 21 teron: sight 0->9, hp 0->180, mana 0->1, build 0->120, gold 0->120, range 0->4, reactAI 0->11, reactHu 0->9 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->6, hp 0->300, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->6 ...
- 24 khadgar: sight 0->9, hp 0->120, mana 0->1, build 0->120, gold 0->120, range 0->6, reactAI 0->11, reactHu 0->9 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 44 turalyon: sight 0->6, hp 0->180, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->7, reactHu 0->5 ...
- 46 danath: sight 0->6, hp 0->220, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 101 portal: sight 0->9, hp 5000->3000, armor 0->20
- 8 archer: hp 40->60, range 4->6, dmg 3->4, pierce 6->7, aup 0->1
- 9 axethrower: hp 40->60, range 4->6, dmg 3->4, pierce 6->7, aup 0->1
- 18 ranger: hp 50->60, range 4->6, dmg 3->4, pierce 6->7, aup 0->1
- 19 berserker: hp 50->60, range 4->6, dmg 3->4, pierce 6->7, aup 0->1
- 60 humbarracks: hp 800->1200
- 61 orcbarracks: hp 800->1200
- 64 humscout: hp 100->500, build 60->30
- 65 orcscout: hp 100->500, build 60->30
- 103 humwall: gold 2->255, lumber 1->255, oil 0->255
- 104 orcwall: gold 2->255, lumber 1->255, oil 0->255
- 45 eye: flags 134217858->130

### u8def v2.1.pud

37 unit types changed.

- 0 footman: sight 4->9, hp 60->210, build 60->1, gold 60->1, armor 2->6, dmg 6->8, pierce 3->8
- 2 peasant: sight 4->9, hp 30->100, dmg 3->0, pierce 2->1
- 6 knight: sight 4->9, hp 90->170, build 90->0, gold 80->3, lumber 10->0, armor 4->7, dmg 8->12, pierce 4->12
- 8 archer: sight 5->9, hp 40->80, build 70->0, gold 50->1, lumber 5->0, dmg 3->0, pierce 6->14, aup 0->1
- 12 paladin: sight 5->9, hp 90->200, build 90->0, gold 80->2, lumber 10->0, armor 4->6, dmg 8->12, pierce 4->10
- 20 alleria: sight 0->9, hp 0->120, build 0->70, gold 0->50, lumber 0->5, range 0->7, reactAI 0->7, reactHu 0->5 ...
- 21 teron: sight 0->9, hp 0->180, mana 0->1, build 0->120, gold 0->120, range 0->4, reactAI 0->11, reactHu 0->9 ...
- 22 kurdran: sight 0->9, hp 0->250, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->6 ...
- 23 dentarg: sight 0->6, hp 0->300, mana 0->1, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->6 ...
- 24 khadgar: sight 0->9, hp 0->120, mana 0->1, build 0->120, gold 0->120, range 0->6, reactAI 0->11, reactHu 0->9 ...
- 25 grom: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 35 u35: sight 0->9, hp 0->800, build 0->250, gold 0->250, range 0->5, reactAI 0->8, reactHu 0->6, armor 0->10 ...
- 44 turalyon: sight 0->6, hp 0->180, build 0->90, gold 0->80, lumber 0->10, range 0->1, reactAI 0->7, reactHu 0->5 ...
- 46 danath: sight 0->6, hp 0->220, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 47 korgath: sight 0->5, hp 0->240, build 0->60, gold 0->60, range 0->1, reactAI 0->6, reactHu 0->4, armor 0->8 ...
- 1 grunt: hp 60->57
- 4 ballista: hp 110->200, build 250->0, gold 90->2, lumber 30->0, pierce 0->10, aup 0->1
- 5 catapult: hp 110->150, dmg 80->100, pierce 0->100
- 7 ogre: hp 90->103
- 9 axethrower: hp 40->77, armor 0->2, dmg 3->7, pierce 6->8
- 13 ogremage: hp 90->133, dmg 8->10, pierce 4->8
- 17 atkpeon: hp 30->27
- 18 ranger: hp 50->140, build 70->0, gold 50->1, lumber 5->0, range 4->5, dmg 3->5, pierce 6->25, aup 0->1
- 49 chogall: hp 100->201, armor 0->6, dmg 10->12, pierce 5->10
- 50 lothar: hp 90->500, reactAI 7->4, armor 4->5, prio 65->255, dmg 8->10, pierce 4->7, annoy 0->10
- 57 critter: hp 5->9
- 76 elvlumber: hp 600->33, build 150->5, gold 60->1, lumber 45->0, oil 0->10
- 92 goldmine: hp 25500->255
- 102 runestone: hp 5000->70
- 26 humtanker: build 50->0, gold 40->1, lumber 20->2, oil 0->10
- 30 elvdestroyer: build 90->0, gold 70->1, lumber 35->20, oil 70->150
- 58 farm: build 100->8
- 59 pigfarm: build 100->8
- 64 humscout: build 60->10, gold 55->0, lumber 20->1
- 96 humguard: build 140->0, gold 50->0, lumber 15->25, range 6->12, pierce 12->2, wup 0->1, aup 0->1
- 98 humcannon: build 190->0, gold 100->0, lumber 30->35, range 7->13, dmg 50->24, wup 0->1, aup 0->1, cantarget 3->7
- 45 eye: flags 134217858->130

## ALOW

6 arrays x 16 players x 32-bit masks (384 bytes). Meaning of each array is NOT verified; shown raw, player 0 and whether all 16 players are equal.

| map | arr0 | arr1 | arr2 | arr3 | arr4 | arr5 | players differ |
|---|---|---|---|---|---|---|---|
| maps\mutton thinned.pud | (no ALOW: engine defaults) |||||||
| maps\sheep tag v1.pud | (no ALOW: engine defaults) |||||||
| maps\sheep tag v2.pud | (no ALOW: engine defaults) |||||||
| tr\capture flag v3.pud | 0xf5f9ffff | 0x44d40 | 0xffffffff | 0x0 | 0xffffffff | 0x0 | arrays [0, 1] |
| tr\desert strike v4.pud | 0xfdfe8002 | 0x10000 | 0xffe10000 | 0x0 | 0xfff00000 | 0x0 | arrays [0, 1, 2, 4] |
| tr\edo def v1.4.pud | 0xbe9e0826 | 0xffc2f | 0xffffffff | 0x0 | 0xfffeffcf | 0x0 | arrays [0, 1, 4] |
| tr\hero surv v1.3.pud | 0x8200000b | 0xfffff | 0xffffffff | 0x0 | 0xfff00000 | 0x0 | arrays [0, 1, 4] |
| tr\smart dota v1.4.pud | 0xfbbed83f | 0xfffff | 0xffffffff | 0x0 | 0xffffffff | 0x0 | arrays [0, 1] |
| tr\u8def v2.1.pud | 0x820420dd | 0x4020 | 0xffffffff | 0x0 | 0xfffffc3f | 0x0 | arrays [0, 4] |