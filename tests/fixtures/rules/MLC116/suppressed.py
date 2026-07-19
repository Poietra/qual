from manim import Circle, RIGHT, Scene, Square, Transform


class SuppressedPostTransformTarget(Scene):
    def construct(self):
        source = Square()
        target = Circle()
        self.add(source)
        self.play(Transform(source, target))
        self.play(target.animate.shift(RIGHT))  # manim-lint: ignore[MLC116]
