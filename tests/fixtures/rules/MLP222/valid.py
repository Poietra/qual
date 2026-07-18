from manim import RIGHT, ImageMobject, Scene, Square


class Demo(Scene):
    def construct(self):
        photo = ImageMobject("photo.png")
        mover = Square()
        self.add(photo, mover)
        self.play(mover.animate.shift(RIGHT))
