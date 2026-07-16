Mission POC (aucun chiffre ne compte pour le banc — protocole non gelé, n=1,
delib-20260707-50f5) : dérivation en 8 marches sur un oscillateur anharmonique
MUTÉ, chaque marche vérifiable par un oracle SymPy exécutable.

PROBLÈME. Un oscillateur quantique 1D de hamiltonien
H = p²/(2m) + (1/2)·m·ω²·x² + λ·x⁶, avec ħ = 1, m = 2, ω = 3, et λ > 0 petit
(perturbation). Les paramètres sont volontairement hors-manuel : ne récite pas
un résultat de cours, dérive avec CES valeurs. Traite λ symboliquement
(symbole `lam`) ; théorie des perturbations de Rayleigh-Schrödinger sur la
base propre de l'oscillateur harmonique non perturbé.

LES 8 MARCHES (chacune = UNE quantité, exacte sauf S8) :
S1: énergie du fondamental non perturbé E₀⁽⁰⁾.
S2: ⟨0|x²|0⟩ dans le fondamental non perturbé.
S3: ⟨0|x⁶|0⟩ dans le fondamental non perturbé.
S4: correction d'énergie du fondamental au 1er ordre en λ, E₀⁽¹⁾.
S5: ⟨1|x⁶|1⟩ dans le premier état excité non perturbé.
S6: énergie de transition E₁ − E₀ au 1er ordre en λ (incluant le terme d'ordre 0).
S7: correction du fondamental au 2ᵉ ordre en λ, E₀⁽²⁾ (somme sur les états
    intermédiaires n ≠ 0 couplés par x⁶ ; attention au signe).
S8: valeur numérique de E₀⁽⁰⁾ + E₀⁽¹⁾ + E₀⁽²⁾ pour λ = 1/10, à 6 chiffres
    significatifs.

FORMAT DE SORTIE (obligatoire — l'oracle est un programme, pas un lecteur).
Le livrable FINAL doit contenir, tel quel, un bloc :

ANSWERS
S1: <expression>
S2: <expression>
S3: <expression>
S4: <expression>
S5: <expression>
S6: <expression>
S7: <expression>
S8: <nombre décimal>

où chaque <expression> est parsable par sympy.sympify : rationnels exacts
(ex. `3/2`), le symbole `lam` pour λ (ex. `5*lam/576`), syntaxe Python
(`**` pour la puissance, jamais `^`). S8 est un décimal (ex. `1.23456`).
Pas d'unités, pas de LaTeX, pas de mise en forme dans le bloc.

Si tu disposes d'outils d'exécution, vérifie tes marches avec SymPy
(python3 -c) avant de les inscrire. Montre la dérivation (les formules
utilisées, pas seulement les valeurs) dans le corps de ta réponse, puis
termine par le bloc ANSWERS.

Si tu es un rôle d'une flotte (indiqué dans le topic) : fais uniquement la
part de TON rôle sur cette mission ; les rôles qui produisent ou raffinent la
dérivation reportent le bloc ANSWERS complet (au meilleur état connu) dans
leur synthèse ; les rôles advisory/arbitration critiquent ou tranchent sans
recalculer toute la dérivation.

INTERDIT : lancer le banc, la flotte ou tout script d'expérience
(run-comparison.sh, run-m4-smoke.sh, academy-run, cargo run, cs nucleate,
cs tackle) — ton livrable est du texte (et des vérifications SymPy locales),
jamais une exécution du harnais.
