import manim as mn


class AliasPostTransformTarget(mn.Scene):
    def construct(self):
        source = mn.Square()
        target = mn.Circle()
        self.add(source)
        self.play(mn.Transform(source, target))
        self.play(target.animate.shift(mn.RIGHT))
