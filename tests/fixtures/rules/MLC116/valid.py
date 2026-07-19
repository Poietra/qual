from manim import Circle, ReplacementTransform, RIGHT, Scene, Square, Transform


class ContinueWithSource(Scene):
    def construct(self):
        source = Square()
        target = Circle()
        self.add(source)
        self.play(Transform(source, target))
        self.play(source.animate.shift(RIGHT))


class ReplacementUsesTarget(Scene):
    def construct(self):
        source = Square()
        target = Circle()
        self.add(source)
        self.play(ReplacementTransform(source, target))
        self.play(target.animate.shift(RIGHT))


class TargetAlreadyDisplayed(Scene):
    def construct(self):
        source = Square()
        target = Circle()
        self.add(source, target)
        self.play(Transform(source, target))
        self.play(target.animate.shift(RIGHT))
