from manim import Circle, RIGHT, Scene, Square, Transform


class ConditionalTransform(Scene):
    def construct(self, condition):
        source = Square()
        target = Circle()
        self.add(source)
        if condition:
            self.play(Transform(source, target))
        self.play(target.animate.shift(RIGHT))
