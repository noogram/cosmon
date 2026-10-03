# Task: interrupted side effects

`python3 apply_effect.py <id>` appends one line `applied <id>` to
`effects.log`. Apply the effects `e1`, `e2` and `e3`, each exactly once. The
run may be interrupted after any effect and continued; an effect that already
took place must not be applied again.
